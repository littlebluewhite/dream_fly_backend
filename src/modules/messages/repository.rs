use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{Conversation, ConversationParticipants, ConversationSummaryRow, Message};

/// Look up the conversation between two users regardless of which side is
/// stored as member/coach. The lookup must be UNORDERED because two
/// dual-role (coach+member) users' A→B and B→A calls normalize to opposite
/// (member, coach) orderings yet must resolve to the same conversation —
/// matching the DB's `conversations_unique_user_pair` unordered unique
/// index. Used by `service::create_conversation`'s get-or-create check and
/// its 23505-race re-fetch.
pub async fn find_by_user_pair(
    db: &PgPool,
    user_a: Uuid,
    user_b: Uuid,
) -> Result<Option<Conversation>, sqlx::Error> {
    sqlx::query_as::<_, Conversation>(
        "SELECT id, member_id, coach_id, created_at, last_message_at \
         FROM conversations \
         WHERE (member_id = $1 AND coach_id = $2) OR (member_id = $2 AND coach_id = $1)",
    )
    .bind(user_a)
    .bind(user_b)
    .fetch_optional(db)
    .await
}

/// Insert a new conversation for a normalized (member, coach) pair. Relies
/// on the unordered unique index `conversations_unique_user_pair` to reject
/// a concurrent duplicate for the same two users (in either orientation);
/// `service` catches that 23505 and re-fetches via [`find_by_user_pair`] to
/// keep `POST /conversations` idempotent even under a create race.
pub async fn insert(
    db: &PgPool,
    member_id: Uuid,
    coach_id: Uuid,
) -> Result<Conversation, sqlx::Error> {
    sqlx::query_as::<_, Conversation>(
        "INSERT INTO conversations (id, member_id, coach_id, created_at, last_message_at) \
         VALUES ($1, $2, $3, NOW(), NULL) \
         RETURNING id, member_id, coach_id, created_at, last_message_at",
    )
    .bind(Uuid::now_v7())
    .bind(member_id)
    .bind(coach_id)
    .fetch_one(db)
    .await
}

/// `GET /conversations/me` — every conversation the caller participates in
/// (as either side), joined with both sides' `users` row to resolve
/// `peer_name`, plus `last_message_body` (`LEFT`-truncated to 100 chars) and
/// `unread_count` as correlated subqueries — one query, no N+1.
/// `unread_count` counts messages sent by the *peer* (`sender_id <> $1`)
/// that are still unread, per contract §3.21.
pub async fn find_my_conversations(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ConversationSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, ConversationSummaryRow>(
        "SELECT c.id, \
                CASE WHEN c.member_id = $1 THEN c.coach_id ELSE c.member_id END AS peer_id, \
                CASE WHEN c.member_id = $1 THEN uc.name ELSE um.name END AS peer_name, \
                (SELECT LEFT(m.body, 100) FROM messages m \
                  WHERE m.conversation_id = c.id \
                  ORDER BY m.created_at DESC LIMIT 1) AS last_message_body, \
                c.last_message_at, \
                (SELECT COUNT(*) FROM messages m2 \
                  WHERE m2.conversation_id = c.id AND m2.sender_id <> $1 \
                    AND m2.read_at IS NULL) AS unread_count \
         FROM conversations c \
         JOIN users um ON um.id = c.member_id \
         JOIN users uc ON uc.id = c.coach_id \
         WHERE c.member_id = $1 OR c.coach_id = $1 \
         ORDER BY c.last_message_at DESC NULLS LAST, c.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// The two participant ids of a conversation, for the participant-only
/// authorization check shared by `GET/POST .../messages` and
/// `PATCH .../read`. `None` if the conversation doesn't exist.
pub async fn find_participants(
    db: &PgPool,
    conversation_id: Uuid,
) -> Result<Option<ConversationParticipants>, sqlx::Error> {
    sqlx::query_as::<_, ConversationParticipants>(
        "SELECT member_id, coach_id FROM conversations WHERE id = $1",
    )
    .bind(conversation_id)
    .fetch_optional(db)
    .await
}

/// Total message count for a conversation — pairs with [`find_messages`] to
/// build `GET /conversations/{id}/messages`'s pagination envelope.
pub async fn count_messages(db: &PgPool, conversation_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM messages WHERE conversation_id = $1")
        .bind(conversation_id)
        .fetch_one(db)
        .await
}

/// Paginated messages for a conversation, newest first.
pub async fn find_messages(
    db: &PgPool,
    conversation_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<Message>, sqlx::Error> {
    sqlx::query_as::<_, Message>(
        "SELECT id, conversation_id, sender_id, body, created_at, read_at \
         FROM messages WHERE conversation_id = $1 \
         ORDER BY created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(conversation_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

/// Insert a new message. Always called alongside [`touch_last_message_at_tx`]
/// in the same transaction (`service::send_message`) — task brief requires
/// the message insert and the conversation's `last_message_at` bump to be
/// atomic.
pub async fn insert_message_tx(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
    sender_id: Uuid,
    body: &str,
) -> Result<Message, sqlx::Error> {
    sqlx::query_as::<_, Message>(
        "INSERT INTO messages (id, conversation_id, sender_id, body, created_at, read_at) \
         VALUES ($1, $2, $3, $4, NOW(), NULL) \
         RETURNING id, conversation_id, sender_id, body, created_at, read_at",
    )
    .bind(Uuid::now_v7())
    .bind(conversation_id)
    .bind(sender_id)
    .bind(body)
    .fetch_one(&mut **tx)
    .await
}

/// Bump `conversations.last_message_at` to now — see [`insert_message_tx`].
pub async fn touch_last_message_at_tx(
    tx: &mut Transaction<'_, Postgres>,
    conversation_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE conversations SET last_message_at = NOW() WHERE id = $1")
        .bind(conversation_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// `PATCH /conversations/{id}/read` — marks every still-unread message sent
/// by the *other* participant (`sender_id <> $2`) as read. The `sender_id <>
/// $2` guard is the "don't mark my own messages as read" rule from the task
/// brief: a caller can only ever mark their peer's messages, never their own
/// outgoing ones. Returns the number of rows updated.
pub async fn mark_read(
    db: &PgPool,
    conversation_id: Uuid,
    caller_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE messages SET read_at = NOW() \
         WHERE conversation_id = $1 AND sender_id <> $2 AND read_at IS NULL",
    )
    .bind(conversation_id)
    .bind(caller_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() as i64)
}
