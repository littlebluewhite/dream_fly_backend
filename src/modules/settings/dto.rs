use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use validator::Validate;

/// Flat key→value map — the shape returned by both `GET /settings` and
/// `PUT /settings` (the latter echoes the full post-update state). Callers
/// look up by key, so the map itself carries no ordering guarantee beyond
/// `BTreeMap`'s alphabetical iteration — chosen over `HashMap` purely for
/// deterministic serialized JSON (stable test/log output), not a contract
/// requirement.
#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub settings: BTreeMap<String, serde_json::Value>,
}

/// `PUT /settings` body. Each entry in `settings` is upserted independently
/// (partial update, not a full replace) — keys absent from this map are left
/// untouched. `key` is a free-form string and `value` any JSON value; no
/// per-key schema validation (`serde` already guarantees well-formed JSON,
/// see the module doc / integration contract §3.25 for the frontend's
/// documented — but not backend-enforced — convention keys). An empty
/// `settings` map is accepted as a no-op rather than rejected with 400 — see
/// `service::update_settings`'s doc comment for the rationale.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateSettingsRequest {
    pub settings: BTreeMap<String, serde_json::Value>,
}
