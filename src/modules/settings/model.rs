use chrono::{DateTime, Utc};
use serde::Serialize;

/// One row of the global key-value settings table (Round 4 Task B6). No
/// `id`/`created_at` — `key` itself is the primary key, matching the
/// migration's minimal 3-column schema.
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Setting {
    pub key: String,
    pub value: serde_json::Value,
    pub updated_at: DateTime<Utc>,
}
