use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::{Validate, ValidationError};

use crate::extractors::pagination::PageMeta;

use super::model::PointLedgerEntry;

#[derive(Debug, Serialize)]
pub struct LedgerEntryResponse {
    pub id: Uuid,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    pub order_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl From<PointLedgerEntry> for LedgerEntryResponse {
    fn from(e: PointLedgerEntry) -> Self {
        Self {
            id: e.id,
            delta: e.delta,
            balance_after: e.balance_after,
            reason: e.reason.as_str().to_string(),
            order_id: e.order_id,
            created_at: e.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PointsMeResponse {
    pub balance: i64,
    pub ledger: Vec<LedgerEntryResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

/// `POST /points/adjustments` request body (Step 10f, admin-only) — closes
/// the refund/cancel compensation "點數不足" 409 repair loop, see
/// `service::adjust_points` for the full CAS write-up. All three fields are
/// required (a missing field fails JSON deserialization before `Validate`
/// even runs, i.e. still a 422 via `ValidatedJson`'s `JsonRejection` arm).
/// `delta` must be non-zero — rejected here rather than left to fall
/// through to `apply_delta_tx`'s own zero-delta guard, so a malformed
/// request never reaches the lock/compare step at all.
#[derive(Debug, Deserialize, Validate)]
pub struct AdjustPointsRequest {
    pub user_id: Uuid,
    #[validate(custom(function = "validate_nonzero_delta"))]
    pub delta: i64,
    pub expected_balance: i64,
}

fn validate_nonzero_delta(delta: i64) -> Result<(), ValidationError> {
    if delta == 0 {
        let mut err = ValidationError::new("delta_must_be_nonzero");
        err.message = Some("delta must be non-zero".into());
        Err(err)
    } else {
        Ok(())
    }
}

/// `POST /points/adjustments` response — echoes the adjusted user's id
/// (the caller is the admin, not the balance owner, so unlike
/// `PointsMeResponse` there's no implicit "whose balance" from auth
/// context) plus the resulting balance. Deliberately minimal (no ledger
/// entry payload): `GET /points/me` is self-only (bound to the caller's
/// own `auth.user_id`), so there's no existing admin-facing endpoint this
/// response could redundantly duplicate — an admin re-checking a target
/// user's balance later uses `GET /users/{id}`'s `points_balance`
/// (`users::dto::UserResponse`); confirming the exact `AdminAdjust`
/// ledger row still means a direct `point_ledger` query.
#[derive(Debug, Serialize)]
pub struct PointsAdjustmentResponse {
    pub user_id: Uuid,
    pub balance: i64,
}
