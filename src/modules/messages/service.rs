use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::{PageMeta, PaginationParams};
use crate::modules::permissions::repository as permissions_repository;

use super::dto::{
    ConversationResponse, ConversationSummaryResponse, CreateConversationRequest,
    CreateMessageRequest, MessageListResponse, MessageResponse,
};
use super::model::ConversationParticipants;
use super::pairing;
use super::repository;

/// Resolve `(member_id, coach_id)` for a conversation between the caller and
/// `target_id`. Self-rejection is checked here, before the DB round trip —
/// see `pairing`'s module doc for why that precedence matters (a self
/// request must 422 regardless of whether the DB call would succeed, and
/// must not spend a query on a request that's rejected either way). The
/// caller's roles come from the already-loaded `AuthUser` (no DB round
/// trip); the target's roles are the one DB dependency here, since they
/// aren't the authenticated party.
async fn resolve_member_coach(
    db: &PgPool,
    auth: &AuthUser,
    target_id: Uuid,
) -> Result<(Uuid, Uuid), AppError> {
    if target_id == auth.user_id {
        return Err(AppError::Validation(pairing::ROLE_VIOLATION.into()));
    }

    let target_roles = permissions_repository::find_role_names_by_user(db, target_id).await?;

    pairing::resolve_pair(auth.user_id, &auth.roles, target_id, &target_roles)
}

/// `POST /conversations` — get-or-create. Returns the existing conversation
/// between these two users if one exists — looked up as an UNORDERED pair,
/// because two dual-role users' A→B and B→A calls normalize to opposite
/// (member, coach) orientations yet must share one conversation — otherwise
/// creates it. A unique-violation on insert (concurrent create race against
/// the unordered `conversations_unique_user_pair` index) is caught and
/// resolved by re-fetching the row a competing request just inserted, so the
/// endpoint stays idempotent even under a race.
pub async fn create_conversation(
    db: &PgPool,
    auth: &AuthUser,
    req: CreateConversationRequest,
) -> Result<ConversationResponse, AppError> {
    let (member_id, coach_id) = resolve_member_coach(db, auth, req.user_id).await?;

    if let Some(existing) = repository::find_by_user_pair(db, member_id, coach_id).await? {
        return Ok(ConversationResponse::from(existing));
    }

    match repository::insert(db, member_id, coach_id).await {
        Ok(conv) => Ok(ConversationResponse::from(conv)),
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            let existing = repository::find_by_user_pair(db, member_id, coach_id)
                .await?
                .ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!(
                        "conversation ({member_id}, {coach_id}) vanished after unique violation"
                    ))
                })?;
            Ok(ConversationResponse::from(existing))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

/// `GET /conversations/me` — plain array (mirrors `leave-requests/me`'s
/// `/me` convention: no pagination), newest-conversation-first.
pub async fn list_my_conversations(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ConversationSummaryResponse>, AppError> {
    let rows = repository::find_my_conversations(db, user_id).await?;
    Ok(rows
        .into_iter()
        .map(ConversationSummaryResponse::from)
        .collect())
}

/// Shared participant gate for `GET/POST .../messages` and
/// `PATCH .../read`: 404 if the conversation doesn't exist, 403 if it exists
/// but the caller is neither side.
async fn authorize_participant(
    db: &PgPool,
    auth: &AuthUser,
    conversation_id: Uuid,
) -> Result<ConversationParticipants, AppError> {
    let participants = repository::find_participants(db, conversation_id)
        .await?
        .ok_or_else(|| AppError::NotFound("對話不存在".into()))?;

    if participants.member_id == auth.user_id || participants.coach_id == auth.user_id {
        Ok(participants)
    } else {
        Err(AppError::Forbidden("非此對話參與者".into()))
    }
}

/// `GET /conversations/{id}/messages` — participant-only, paginated
/// `created_at DESC`.
pub async fn list_messages(
    db: &PgPool,
    auth: &AuthUser,
    conversation_id: Uuid,
    pagination: &PaginationParams,
) -> Result<MessageListResponse, AppError> {
    authorize_participant(db, auth, conversation_id).await?;

    let limit = pagination.limit();
    let total = repository::count_messages(db, conversation_id).await?;
    let rows = repository::find_messages(db, conversation_id, limit, pagination.offset()).await?;

    Ok(MessageListResponse {
        messages: rows.into_iter().map(MessageResponse::from).collect(),
        meta: PageMeta {
            total,
            page: pagination.page.max(1),
            per_page: limit,
        },
    })
}

/// `POST /conversations/{id}/messages` — participant-only. Inserts the
/// message and bumps `conversations.last_message_at` in the same transaction
/// (task brief requirement).
pub async fn send_message(
    db: &PgPool,
    auth: &AuthUser,
    conversation_id: Uuid,
    req: CreateMessageRequest,
) -> Result<MessageResponse, AppError> {
    authorize_participant(db, auth, conversation_id).await?;

    let mut tx = db.begin().await?;
    let message =
        repository::insert_message_tx(&mut tx, conversation_id, auth.user_id, &req.body).await?;
    repository::touch_last_message_at_tx(&mut tx, conversation_id).await?;
    tx.commit().await?;

    Ok(MessageResponse::from(message))
}

/// `PATCH /conversations/{id}/read` — participant-only. Marks every
/// still-unread message sent by the *peer* as read; never touches the
/// caller's own messages (see `repository::mark_read`).
pub async fn mark_read(
    db: &PgPool,
    auth: &AuthUser,
    conversation_id: Uuid,
) -> Result<i64, AppError> {
    authorize_participant(db, auth, conversation_id).await?;
    let updated = repository::mark_read(db, conversation_id, auth.user_id).await?;
    Ok(updated)
}
