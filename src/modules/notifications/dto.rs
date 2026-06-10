use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::model::Notification;

#[derive(Debug, Serialize)]
pub struct NotificationResponse {
    pub id: Uuid,
    #[serde(rename = "type")]
    pub notification_type: String,
    pub title: String,
    pub message: String,
    pub is_read: bool,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

impl From<Notification> for NotificationResponse {
    fn from(n: Notification) -> Self {
        Self {
            id: n.id,
            notification_type: n.notification_type.as_str().to_string(),
            title: n.title,
            message: n.message,
            is_read: n.is_read,
            metadata: n.metadata,
            created_at: n.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UnreadCountResponse {
    pub count: i64,
}
