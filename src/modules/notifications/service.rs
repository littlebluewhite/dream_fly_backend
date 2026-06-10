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

pub async fn get_unread_count(
    db: &PgPool,
    user_id: Uuid,
) -> Result<UnreadCountResponse, AppError> {
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
