use sqlx::PgPool;
use uuid::Uuid;

use super::model::{Notification, NotificationType};

pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<Notification>, sqlx::Error> {
    sqlx::query_as::<_, Notification>(
        "SELECT * FROM notifications \
         WHERE user_id = $1 \
         ORDER BY created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_unread(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND is_read = false",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

pub async fn mark_read(
    db: &PgPool,
    id: Uuid,
    user_id: Uuid,
) -> Result<Option<Notification>, sqlx::Error> {
    sqlx::query_as::<_, Notification>(
        "UPDATE notifications SET is_read = true \
         WHERE id = $1 AND user_id = $2 \
         RETURNING *",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

pub async fn create_notification(
    db: &PgPool,
    user_id: Uuid,
    notification_type: &NotificationType,
    title: &str,
    message: &str,
    metadata: Option<serde_json::Value>,
) -> Result<Notification, sqlx::Error> {
    sqlx::query_as::<_, Notification>(
        "INSERT INTO notifications (id, user_id, \"type\", title, message, metadata) \
         VALUES (gen_random_uuid(), $1, $2::notification_type, $3, $4, $5) \
         RETURNING *",
    )
    .bind(user_id)
    .bind(notification_type.as_str())
    .bind(title)
    .bind(message)
    .bind(metadata)
    .fetch_one(db)
    .await
}
