use sqlx::PgPool;

use crate::error::AppError;

use super::dto::{SettingsResponse, UpdateSettingsRequest};
use super::model::Setting;
use super::repository;

fn to_response(rows: Vec<Setting>) -> SettingsResponse {
    SettingsResponse {
        settings: rows.into_iter().map(|r| (r.key, r.value)).collect(),
    }
}

/// `GET /settings` — admin-only (checked by the handler). An empty table
/// yields `{ "settings": {} }`, not an error.
pub async fn get_settings(db: &PgPool) -> Result<SettingsResponse, AppError> {
    let rows = repository::find_all(db).await?;
    Ok(to_response(rows))
}

/// `PUT /settings` — admin-only (checked by the handler). Upserts every key
/// in `req.settings` inside a single transaction (brief requirement); keys
/// absent from the body are left untouched — this is a partial update, never
/// a full replace.
///
/// An empty `settings` map opens and commits a transaction with zero writes
/// (a no-op) rather than being rejected with 400: "upsert only the keys
/// sent" already implies zero keys sent means zero writes, so no extra
/// special-case validation is needed to reach that behavior — it falls out
/// of the loop doing nothing. Returns the full post-update state (same shape
/// as `get_settings`), matching the empty-body no-op-but-still-200
/// convention documented in integration-contract.md §3.25.
pub async fn update_settings(
    db: &PgPool,
    req: UpdateSettingsRequest,
) -> Result<SettingsResponse, AppError> {
    let mut tx = db.begin().await?;
    for (key, value) in &req.settings {
        repository::upsert_tx(&mut tx, key, value).await?;
    }
    tx.commit().await?;

    let rows = repository::find_all(db).await?;
    Ok(to_response(rows))
}
