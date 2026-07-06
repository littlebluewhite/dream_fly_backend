use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Bare `conversations` table row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Conversation {
    pub id: Uuid,
    pub member_id: Uuid,
    pub coach_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
}

/// Bare `messages` table row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Message {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub sender_id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

/// Just the two participant ids of a conversation — used by every
/// participant-only endpoint (`GET/POST .../messages`, `PATCH .../read`) to
/// check "is this caller one of the two people in this conversation" without
/// pulling back the whole row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConversationParticipants {
    pub member_id: Uuid,
    pub coach_id: Uuid,
}

/// One row of `GET /conversations/me` — `conversations` JOINed with both
/// sides' `users` row (to resolve whichever one isn't the caller as
/// `peer_name`) plus two correlated-subquery aggregates, `last_message_body`
/// (already `LEFT`-truncated to 100 chars in SQL) and `unread_count`
/// (messages in this conversation sent by the peer with `read_at IS NULL`).
/// One query for the whole list, no N+1 — see
/// `repository::find_my_conversations`.
#[derive(Debug, sqlx::FromRow)]
pub struct ConversationSummaryRow {
    pub id: Uuid,
    pub peer_id: Uuid,
    pub peer_name: String,
    pub last_message_body: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub unread_count: i64,
}
