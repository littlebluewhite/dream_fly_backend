use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;

use super::model::{Conversation, ConversationSummaryRow, Message};

// ---------------------------------------------------------------------------
// POST /conversations
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct CreateConversationRequest {
    pub user_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub member_id: Uuid,
    pub coach_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
}

impl From<Conversation> for ConversationResponse {
    fn from(c: Conversation) -> Self {
        Self {
            id: c.id,
            member_id: c.member_id,
            coach_id: c.coach_id,
            created_at: c.created_at,
            last_message_at: c.last_message_at,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /conversations/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ConversationSummaryResponse {
    pub id: Uuid,
    pub peer_id: Uuid,
    pub peer_name: String,
    pub last_message_body: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub unread_count: i64,
}

impl From<ConversationSummaryRow> for ConversationSummaryResponse {
    fn from(r: ConversationSummaryRow) -> Self {
        Self {
            id: r.id,
            peer_id: r.peer_id,
            peer_name: r.peer_name,
            last_message_body: r.last_message_body,
            last_message_at: r.last_message_at,
            unread_count: r.unread_count,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /conversations/{id}/messages
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: Uuid,
    pub sender_id: Uuid,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

impl From<Message> for MessageResponse {
    fn from(m: Message) -> Self {
        Self {
            id: m.id,
            sender_id: m.sender_id,
            body: m.body,
            created_at: m.created_at,
            read_at: m.read_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MessageListResponse {
    pub messages: Vec<MessageResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

// ---------------------------------------------------------------------------
// POST /conversations/{id}/messages
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct CreateMessageRequest {
    #[validate(length(min = 1, max = 2000, message = "訊息內容長度需介於 1 到 2000 字之間"))]
    pub body: String,
}

// ---------------------------------------------------------------------------
// PATCH /conversations/{id}/read
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MarkReadResponse {
    pub updated: i64,
}
