use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{NotificationResponse, UnreadCountResponse};
use super::model::NotificationType;
use super::repository;

pub async fn list_notifications(
    db: &PgPool,
    user_id: Uuid,
    pagination: &PaginationParams,
) -> Result<Vec<NotificationResponse>, AppError> {
    let notifications =
        repository::find_by_user(db, user_id, pagination.limit(), pagination.offset()).await?;
    Ok(notifications
        .into_iter()
        .map(NotificationResponse::from)
        .collect())
}

pub async fn get_unread_count(db: &PgPool, user_id: Uuid) -> Result<UnreadCountResponse, AppError> {
    let count = repository::count_unread(db, user_id).await?;
    Ok(UnreadCountResponse { count })
}

pub async fn mark_as_read(
    db: &PgPool,
    id: Uuid,
    user_id: Uuid,
) -> Result<NotificationResponse, AppError> {
    let notification = repository::mark_read(db, id, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("notification not found".into()))?;
    Ok(NotificationResponse::from(notification))
}

struct NotificationContent {
    notif_type: NotificationType,
    title: &'static str,
    message: String,
    metadata: Option<serde_json::Value>,
}

fn booking_confirmed_content(booking_id: Uuid) -> NotificationContent {
    NotificationContent {
        notif_type: NotificationType::BookingConfirmed,
        title: "Booking Confirmed",
        message: "Your booking has been confirmed.".to_string(),
        metadata: Some(serde_json::json!({"booking_id": booking_id})),
    }
}

fn booking_cancelled_content(booking_id: Uuid) -> NotificationContent {
    NotificationContent {
        notif_type: NotificationType::BookingCancelled,
        title: "Booking Cancelled",
        message: "Your booking has been cancelled.".to_string(),
        metadata: Some(serde_json::json!({"booking_id": booking_id})),
    }
}

fn order_placed_content(order_id: Uuid, order_number: &str) -> NotificationContent {
    NotificationContent {
        notif_type: NotificationType::OrderPlaced,
        title: "Order Placed",
        message: format!("Your order {order_number} has been placed."),
        metadata: Some(serde_json::json!({"order_id": order_id, "order_number": order_number})),
    }
}

fn order_status_changed_content(
    order_id: Uuid,
    order_number: &str,
    status: &str,
) -> NotificationContent {
    NotificationContent {
        notif_type: NotificationType::OrderStatus,
        title: "Order Update",
        message: format!("Your order status has been updated to: {status}"),
        metadata: Some(
            serde_json::json!({"order_id": order_id, "order_number": order_number, "status": status}),
        ),
    }
}

fn user_welcomed_content() -> NotificationContent {
    NotificationContent {
        notif_type: NotificationType::System,
        title: "Welcome to Dream Fly",
        message: "Welcome to Dream Fly! Your account is ready.".to_string(),
        metadata: None,
    }
}

async fn emit(db: &PgPool, user_id: Uuid, c: NotificationContent) {
    if let Err(e) =
        repository::create_notification(db, user_id, &c.notif_type, c.title, &c.message, c.metadata)
            .await
    {
        tracing::error!(error = ?e, notif_type = c.notif_type.as_str(), "failed to write notification");
    }
}

pub async fn booking_confirmed(db: &PgPool, user_id: Uuid, booking_id: Uuid) {
    emit(db, user_id, booking_confirmed_content(booking_id)).await
}

pub async fn booking_cancelled(db: &PgPool, user_id: Uuid, booking_id: Uuid) {
    emit(db, user_id, booking_cancelled_content(booking_id)).await
}

pub async fn order_placed(db: &PgPool, user_id: Uuid, order_id: Uuid, order_number: &str) {
    emit(db, user_id, order_placed_content(order_id, order_number)).await
}

pub async fn order_status_changed(
    db: &PgPool,
    user_id: Uuid,
    order_id: Uuid,
    order_number: &str,
    status: &str,
) {
    emit(
        db,
        user_id,
        order_status_changed_content(order_id, order_number, status),
    )
    .await
}

pub async fn user_welcomed(db: &PgPool, user_id: Uuid) {
    emit(db, user_id, user_welcomed_content()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_booking_confirmed_content() {
        let id = Uuid::new_v4();
        let c = booking_confirmed_content(id);
        assert_eq!(c.notif_type.as_str(), "booking_confirmed");
        assert_eq!(c.title, "Booking Confirmed");
        assert_eq!(c.message, "Your booking has been confirmed.");
        assert_eq!(c.metadata, Some(serde_json::json!({"booking_id": id})));
    }

    #[test]
    fn test_booking_cancelled_content() {
        let id = Uuid::new_v4();
        let c = booking_cancelled_content(id);
        assert_eq!(c.notif_type.as_str(), "booking_cancelled");
        assert_eq!(c.title, "Booking Cancelled");
        assert_eq!(c.message, "Your booking has been cancelled.");
        assert_eq!(c.metadata, Some(serde_json::json!({"booking_id": id})));
    }

    #[test]
    fn test_order_placed_content() {
        let id = Uuid::new_v4();
        let c = order_placed_content(id, "ORD-123");
        assert_eq!(c.notif_type.as_str(), "order_placed");
        assert_eq!(c.title, "Order Placed");
        assert_eq!(c.message, "Your order ORD-123 has been placed.");
        assert_eq!(
            c.metadata,
            Some(serde_json::json!({"order_id": id, "order_number": "ORD-123"}))
        );
    }

    #[test]
    fn test_order_status_changed_content() {
        let id = Uuid::new_v4();
        let c = order_status_changed_content(id, "ORD-123", "shipped");
        assert_eq!(c.notif_type.as_str(), "order_status");
        assert_eq!(c.title, "Order Update");
        assert_eq!(c.message, "Your order status has been updated to: shipped");
        assert_eq!(
            c.metadata,
            Some(
                serde_json::json!({"order_id": id, "order_number": "ORD-123", "status": "shipped"})
            )
        );
    }

    #[test]
    fn test_user_welcomed_content() {
        let c = user_welcomed_content();
        assert_eq!(c.notif_type.as_str(), "system");
        assert_eq!(c.title, "Welcome to Dream Fly");
        assert_eq!(c.message, "Welcome to Dream Fly! Your account is ready.");
        assert_eq!(c.metadata, None);
    }
}

/// Send a booking confirmation notification
pub async fn send_booking_confirmation(
    db: &PgPool,
    user_id: Uuid,
    booking_id: Uuid,
) -> Result<(), AppError> {
    repository::create_notification(
        db,
        user_id,
        &NotificationType::BookingConfirmed,
        "Booking Confirmed",
        "Your booking has been confirmed.",
        Some(serde_json::json!({"booking_id": booking_id})),
    )
    .await?;
    Ok(())
}

/// Send an order status update notification
pub async fn send_order_update(
    db: &PgPool,
    user_id: Uuid,
    order_id: Uuid,
    status: &str,
) -> Result<(), AppError> {
    repository::create_notification(
        db,
        user_id,
        &NotificationType::OrderStatus,
        "Order Update",
        &format!("Your order status has been updated to: {status}"),
        Some(serde_json::json!({"order_id": order_id, "status": status})),
    )
    .await?;
    Ok(())
}
