use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "notification_type", rename_all = "snake_case")]
pub enum NotificationType {
    BookingConfirmed,
    BookingCancelled,
    OrderPlaced,
    OrderStatus,
    System,
    Promotion,
}

impl NotificationType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BookingConfirmed => "booking_confirmed",
            Self::BookingCancelled => "booking_cancelled",
            Self::OrderPlaced => "order_placed",
            Self::OrderStatus => "order_status",
            Self::System => "system",
            Self::Promotion => "promotion",
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
    pub title: String,
    pub message: String,
    pub is_read: bool,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}
