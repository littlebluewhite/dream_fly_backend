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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::notifications::model::NotificationType;

    /// Golden wire shape: `notification_type` goes out under the key `type`.
    #[test]
    fn response_json_is_golden() {
        let n = Notification {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            notification_type: NotificationType::BookingConfirmed,
            title: "t".into(),
            message: "m".into(),
            is_read: false,
            metadata: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        assert_eq!(
            serde_json::to_string(&NotificationResponse::from(n)).unwrap(),
            r#"{"id":"00000000-0000-0000-0000-000000000000","type":"booking_confirmed","title":"t","message":"m","is_read":false,"metadata":null,"created_at":"1970-01-01T00:00:00Z"}"#
        );
    }
}
