use sqlx::{PgPool, Postgres, Transaction};

use super::model::Setting;

/// All rows, unordered beyond whatever Postgres returns by default — the
/// service layer folds this into a `BTreeMap` so response ordering is
/// deterministic regardless of physical row order here.
pub async fn find_all(db: &PgPool) -> Result<Vec<Setting>, sqlx::Error> {
    sqlx::query_as::<_, Setting>("SELECT key, value, updated_at FROM settings")
        .fetch_all(db)
        .await
}

/// Upsert one key within an already-open transaction — `service` loops this
/// once per key in the `PUT /settings` body (mirrors
/// `attendance::repository::upsert_attendance_tx`'s per-row loop-in-tx
/// style), then commits once so the whole request is a single transaction.
pub async fn upsert_tx(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    value: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, now()) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
