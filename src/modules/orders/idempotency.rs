//! Checkout idempotency — the single owner of the `order_idempotency` table.
//!
//! A checkout carrying an `Idempotency-Key` is deduplicated per
//! `(user_id, key)`: a second attempt with the same pair returns the original
//! order instead of creating a duplicate (double-click, network retry, mobile
//! 502 retry). `orders::service::checkout` reaches this module at exactly
//! three named points — `replay` (pre-check), `replay_or` (empty cart) and
//! `record` (unique violation) — and its doc's outcome-priority list stays
//! the single authority on where each one sits. With no key (`None`) all
//! three let the checkout through unchanged.
//!
//! Every replay assembles its response through the pool, so every path that
//! still holds the checkout transaction releases it first
//! (`tx_witness::TxReleased`).

use axum::http::HeaderMap;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

use super::dto::OrderResponse;
use super::repository;
use super::service::assemble_response;
use super::tx_witness::TxReleased;

/// A validated `Idempotency-Key`: 1–128 ASCII printable characters after
/// trimming. The field is private — `from_headers` / `parse` are the only
/// ways to build one.
#[derive(Debug, Clone)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Read the `Idempotency-Key` header. Absence is a legitimate choice to opt
    /// out of replay protection — `Ok(None)`, checkout proceeds unprotected.
    /// Presence with an illegal value is rejected outright (`Err`, 400) rather
    /// than silently downgraded to an unprotected checkout: a client that
    /// *thought* it was sending a valid key deserves to know its request was
    /// not deduplicated, instead of finding out only after a double-submit
    /// created two orders. Header values that are not valid UTF-8 at all
    /// (`HeaderValue::to_str` fails) get the same 400; everything else is
    /// `parse`'s rule.
    pub fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AppError> {
        let Some(value) = headers.get("idempotency-key") else {
            return Ok(None);
        };
        let value = value.to_str().map_err(|_| invalid())?;
        Self::parse(value).map(Some)
    }

    /// Trim, then accept 1–128 characters that are each ASCII graphic, `-`
    /// or `_` — 400 otherwise. We bound the length to prevent a 10MB key from
    /// blowing up our unique index, and reject any non-ASCII/non-printable
    /// characters.
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.len() > 128
            || !trimmed
                .chars()
                .all(|c| c.is_ascii_graphic() || c == '-' || c == '_')
        {
            return Err(invalid());
        }
        Ok(Self(trimmed.to_string()))
    }
}

fn invalid() -> AppError {
    AppError::BadRequest("Idempotency-Key must be 1-128 ASCII printable characters".into())
}

/// Outcome of [`record`]: either the key is now recorded in the still-open
/// checkout transaction (handed back to carry on), or a same-key twin had
/// already committed and its order is the response.
pub(super) enum Recorded {
    Fresh(Transaction<'static, Postgres>),
    TwinWon(OrderResponse),
}

/// Rule #1 of checkout's outcome priority: a key already recorded for this
/// user returns that order, before any other check. Runs before the checkout
/// transaction opens (`TxReleased::no_open_tx`). `Ok(None)` — no key, or no
/// row for `(user_id, key)` yet — means carry on with the checkout.
pub(super) async fn replay(
    db: &PgPool,
    user_id: Uuid,
    key: Option<&IdempotencyKey>,
) -> Result<Option<OrderResponse>, AppError> {
    let Some(key) = key else {
        return Ok(None);
    };
    replay_by_key(db, user_id, &key.0, TxReleased::no_open_tx()).await
}

/// Empty-cart branch: replay a same-key twin that already won this cart, or
/// fail with `otherwise` (checkout's 400「cart is empty」). Takes the checkout
/// tx by value and always ends it (rolled back): with no key it is simply
/// dropped before `otherwise` is returned, exactly as a plain `Err` return
/// would.
///
/// Why a twin can be waiting here: idempotency is scoped per user_id, so two
/// same-key requests first contend on the SAME buyer's `users` row — the
/// unconditional `lock_balance_tx` step that opens
/// `locks::acquire_checkout_locks`, ahead of any cart read — and that is what
/// actually serializes the two checkouts. The loser blocks there until the
/// winner locks the same cart rows, runs the whole checkout, clears the cart,
/// records its key and commits — cart-clear and key-insert share that one
/// transaction — so by the time the loser reaches the empty-cart check the
/// winner's row is committed too. Failing outright here, before checking
/// idempotency, would break the "same key replay returns the first order"
/// contract.
pub(super) async fn replay_or(
    db: &PgPool,
    tx: Transaction<'static, Postgres>,
    user_id: Uuid,
    key: Option<&IdempotencyKey>,
    otherwise: AppError,
) -> Result<OrderResponse, AppError> {
    let released = TxReleased::release(tx);
    let Some(key) = key else {
        return Err(otherwise);
    };
    match replay_by_key(db, user_id, &key.0, released).await? {
        Some(response) => Ok(response),
        None => Err(otherwise),
    }
}

/// Record `(user_id, key) → order_id` inside the checkout transaction, so
/// either both the order and the key persist or neither does; a concurrent
/// retry sees either nothing (and races for the lock) or the committed row.
/// With no key the tx comes straight back as `Fresh`.
///
/// Takes the tx by value so the losing branch cannot forget to release it:
/// on a unique violation a concurrent same-key twin beat us — the tx is
/// released (rolling back this whole checkout) before the winner's order is
/// replayed as `TwinWon`, since the replay's `assemble_response` runs pool
/// queries (`TxReleased`).
///
/// A missing row after the unique violation is `Internal`, not a fall-through,
/// because it is unreachable: a PostgreSQL unique-violation on
/// `order_idempotency` means the *other* transaction that inserted the
/// conflicting row has already committed — an uncommitted conflicting insert
/// would still be holding its row lock, so our own insert would block waiting
/// on it rather than fail immediately. And `order_idempotency` has no DELETE
/// code path anywhere in this codebase, so a committed row is never removed.
/// Together: the row about to be looked up is guaranteed to exist. **If
/// `order_idempotency` ever grows a DELETE path, this argument no longer holds
/// and this branch needs re-auditing.**
pub(super) async fn record(
    db: &PgPool,
    mut tx: Transaction<'static, Postgres>,
    user_id: Uuid,
    key: Option<&IdempotencyKey>,
    order_id: Uuid,
) -> Result<Recorded, AppError> {
    let Some(key) = key else {
        return Ok(Recorded::Fresh(tx));
    };
    match insert_idempotency_tx(&mut tx, user_id, &key.0, order_id).await {
        Ok(()) => Ok(Recorded::Fresh(tx)),
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            let released = TxReleased::release(tx);
            let response = replay_by_key(db, user_id, &key.0, released)
                .await?
                .ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!(
                        "idempotency unique violation but no committed row"
                    ))
                })?;
            Ok(Recorded::TwinWon(response))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

/// Look up the order already recorded for `(user_id, key)` and, if one
/// exists, assemble its full response — the shared body of all three replay
/// points. `Ok(None)` means no row exists yet.
async fn replay_by_key(
    db: &PgPool,
    user_id: Uuid,
    key: &str,
    released: TxReleased,
) -> Result<Option<OrderResponse>, AppError> {
    let Some(existing_id) = find_idempotency(db, user_id, key).await? else {
        return Ok(None);
    };
    let order = repository::find_by_id(db, existing_id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "idempotency row referenced missing order {existing_id}"
            ))
        })?;
    Ok(Some(assemble_response(db, order, released).await?))
}

/// Return the order id associated with a prior (user_id, key) pair, if any.
async fn find_idempotency(
    db: &PgPool,
    user_id: Uuid,
    key: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT order_id FROM order_idempotency \
         WHERE user_id = $1 AND idempotency_key = $2",
    )
    .bind(user_id)
    .bind(key)
    .fetch_optional(db)
    .await
}

async fn insert_idempotency_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    key: &str,
    order_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO order_idempotency (user_id, idempotency_key, order_id, created_at) \
         VALUES ($1, $2, $3, NOW())",
    )
    .bind(user_id)
    .bind(key)
    .bind(order_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const INVALID: &str = "Idempotency-Key must be 1-128 ASCII printable characters";

    /// Build a `HeaderMap` carrying a single `idempotency-key: value` entry.
    fn headers_with_key(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_str(value).expect("test value must be a legal HeaderValue"),
        );
        headers
    }

    fn key_of(headers: &HeaderMap) -> Option<String> {
        IdempotencyKey::from_headers(headers).unwrap().map(|k| k.0)
    }

    fn assert_invalid(result: Result<impl std::fmt::Debug, AppError>) {
        let err = result.expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == INVALID),
            "got: {err:?}"
        );
    }

    #[test]
    fn missing_header_is_ok_none() {
        assert_eq!(key_of(&HeaderMap::new()), None);
    }

    #[test]
    fn legal_key_is_ok_some() {
        let headers = headers_with_key("abc123-_XYZ");
        assert_eq!(key_of(&headers), Some("abc123-_XYZ".to_string()));
    }

    #[test]
    fn legal_key_with_surrounding_whitespace_is_trimmed() {
        let headers = headers_with_key("  abc123  ");
        assert_eq!(key_of(&headers), Some("abc123".to_string()));
    }

    #[test]
    fn all_whitespace_key_is_err() {
        assert_invalid(IdempotencyKey::from_headers(&headers_with_key("   ")));
    }

    #[test]
    fn key_over_128_chars_after_trim_is_err() {
        let key = "a".repeat(129);
        assert_invalid(IdempotencyKey::from_headers(&headers_with_key(&key)));
    }

    #[test]
    fn key_with_internal_whitespace_is_err() {
        assert_invalid(IdempotencyKey::from_headers(&headers_with_key("abc 123")));
    }

    #[test]
    fn non_utf8_bytes_is_err() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_bytes(&[0xFF, 0xFE, 0xFD])
                .expect("raw bytes are a legal HeaderValue"),
        );
        assert_invalid(IdempotencyKey::from_headers(&headers));
    }

    #[test]
    fn parse_accepts_128_chars_rejects_129() {
        let max = "a".repeat(128);
        assert_eq!(IdempotencyKey::parse(&max).unwrap().0, max);
        assert_invalid(IdempotencyKey::parse(&"a".repeat(129)));
    }

    #[test]
    fn parse_length_bound_applies_after_trim() {
        // 128 significant chars padded past 129 bytes is still legal — the
        // bound is on the trimmed key.
        let padded = format!("  {}  ", "a".repeat(128));
        assert_eq!(IdempotencyKey::parse(&padded).unwrap().0, "a".repeat(128));
        assert_invalid(IdempotencyKey::parse(""));
        assert_invalid(IdempotencyKey::parse(" \t "));
    }
}
