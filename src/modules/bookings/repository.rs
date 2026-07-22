use sqlx::PgPool;
use uuid::Uuid;

use super::model::Booking;

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Booking>, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "SELECT id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at \
         FROM bookings WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Transactional lookup with a row-level lock (`FOR UPDATE`) so the cancel
/// path's ownership/status check cannot race another concurrent update.
pub async fn find_by_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Booking>, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "SELECT id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at \
         FROM bookings WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<Booking>, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "SELECT id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at \
         FROM bookings WHERE user_id = $1 \
         ORDER BY created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn find_all(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<Booking>, sqlx::Error> {
    sqlx::query_as::<_, Booking>(
        "SELECT id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at \
         FROM bookings \
         ORDER BY created_at DESC \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_by_user(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM bookings WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

pub async fn count_all(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM bookings")
        .fetch_one(db)
        .await
}
