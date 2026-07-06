use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    ConversationResponse, ConversationSummaryResponse, CreateConversationRequest,
    CreateMessageRequest, MarkReadResponse, MessageListResponse, MessageResponse,
};
use super::service;

/// `POST /conversations` — member or coach; service normalizes and
/// get-or-creates.
#[tracing::instrument(skip_all)]
pub async fn create_conversation(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateConversationRequest>,
) -> Result<Json<ConversationResponse>, AppError> {
    let created = service::create_conversation(&state.db, &auth, req).await?;
    Ok(Json(created))
}

/// `GET /conversations/me` — the caller's own conversations.
#[tracing::instrument(skip_all)]
pub async fn my_conversations(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<ConversationSummaryResponse>>, AppError> {
    let conversations = service::list_my_conversations(&state.db, auth.user_id).await?;
    Ok(Json(conversations))
}

/// `GET /conversations/{id}/messages` — participant-only, paginated.
#[tracing::instrument(skip_all)]
pub async fn list_messages(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(pagination): Query<PaginationParams>,
) -> Result<Json<MessageListResponse>, AppError> {
    let result = service::list_messages(&state.db, &auth, id, &pagination).await?;
    Ok(Json(result))
}

/// `POST /conversations/{id}/messages` — participant-only.
#[tracing::instrument(skip_all)]
pub async fn send_message(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    ValidatedJson(req): ValidatedJson<CreateMessageRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let created = service::send_message(&state.db, &auth, id, req).await?;
    Ok(Json(created))
}

/// `PATCH /conversations/{id}/read` — participant-only.
#[tracing::instrument(skip_all)]
pub async fn mark_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<MarkReadResponse>, AppError> {
    let updated = service::mark_read(&state.db, &auth, id).await?;
    Ok(Json(MarkReadResponse { updated }))
}
