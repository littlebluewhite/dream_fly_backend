use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::extractors::request_id::RequestId;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    AdminOrderListResponse, CheckoutRequest, OrderListResponse, OrderResponse,
    UpdateOrderStatusRequest,
};
use super::service;

/// Read the `Idempotency-Key` header. Absence is a legitimate choice to opt
/// out of replay protection — `Ok(None)`, checkout proceeds unprotected.
/// Presence with an illegal value is rejected outright (`Err`, 400) rather
/// than silently downgraded to an unprotected checkout: a client that
/// *thought* it was sending a valid key deserves to know its request was
/// not deduplicated, instead of finding out only after a double-submit
/// created two orders. We bound the length to prevent a 10MB key from
/// blowing up our unique index, and reject any non-ASCII/non-printable
/// characters — including header values that are not valid UTF-8 at all,
/// which `HeaderValue::to_str` surfaces as an error.
fn extract_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let invalid = || {
        AppError::BadRequest(
            "Idempotency-Key must be 1-128 ASCII printable characters".into(),
        )
    };
    let value = value.to_str().map_err(|_| invalid())?;
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 128
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_graphic() || c == '-' || c == '_')
    {
        return Err(invalid());
    }
    Ok(Some(trimmed.to_string()))
}

#[tracing::instrument(skip_all)]
pub async fn checkout(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    request_id: RequestId,
    // `Option<Json<T>>` rather than `ValidatedJson<T>`: axum's built-in
    // `OptionalFromRequest` impl for `Json` yields `None` when the request
    // has no `Content-Type` header at all (the existing no-body `POST
    // /orders` calls), instead of failing extraction the way a bare
    // `ValidatedJson<CheckoutRequest>` would. A present-but-non-JSON
    // content type still errors; a present JSON body (including `{}`, since
    // every `CheckoutRequest` field is `Option`) is parsed normally. This
    // must be the last handler argument (only one extractor per handler may
    // consume the body).
    body: Option<Json<CheckoutRequest>>,
) -> Result<Json<OrderResponse>, AppError> {
    let idempotency_key = extract_idempotency_key(&headers)?;
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let order = service::checkout(
        &state.db,
        auth.user_id,
        idempotency_key,
        req,
        request_id.0,
        &state.config.server,
        state.clock.now(),
    )
    .await?;
    Ok(Json(order))
}

#[tracing::instrument(skip_all)]
pub async fn my_orders(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<OrderListResponse>, AppError> {
    let list = service::my_orders(&state.db, auth.user_id, &params).await?;
    Ok(Json(list))
}

#[tracing::instrument(skip_all)]
pub async fn get_order(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderResponse>, AppError> {
    let order = service::get_order(&state.db, id, &auth).await?;
    Ok(Json(order))
}

#[tracing::instrument(skip_all)]
pub async fn update_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    request_id: RequestId,
    ValidatedJson(req): ValidatedJson<UpdateOrderStatusRequest>,
) -> Result<Json<OrderResponse>, AppError> {
    let order = service::update_order_status(&state.db, id, &req.status, request_id.0).await?;
    Ok(Json(order))
}

/// Paginated order list across all users (admin only).
#[tracing::instrument(skip_all)]
pub async fn admin_list_orders(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<AdminOrderListResponse>, AppError> {
    let result = service::list_all_orders(&state.db, &params).await?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// Build a `HeaderMap` carrying a single `idempotency-key: value` entry.
    fn headers_with_key(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_str(value).expect("test value must be a legal HeaderValue"),
        );
        headers
    }

    #[test]
    fn missing_header_is_ok_none() {
        let headers = HeaderMap::new();
        assert_eq!(extract_idempotency_key(&headers).unwrap(), None);
    }

    #[test]
    fn legal_key_is_ok_some() {
        let headers = headers_with_key("abc123-_XYZ");
        assert_eq!(
            extract_idempotency_key(&headers).unwrap(),
            Some("abc123-_XYZ".to_string())
        );
    }

    #[test]
    fn legal_key_with_surrounding_whitespace_is_trimmed() {
        let headers = headers_with_key("  abc123  ");
        assert_eq!(
            extract_idempotency_key(&headers).unwrap(),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn all_whitespace_key_is_err() {
        let headers = headers_with_key("   ");
        let err = extract_idempotency_key(&headers).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "Idempotency-Key must be 1-128 ASCII printable characters"),
            "got: {err:?}"
        );
    }

    #[test]
    fn key_over_128_chars_after_trim_is_err() {
        let key = "a".repeat(129);
        let headers = headers_with_key(&key);
        let err = extract_idempotency_key(&headers).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "Idempotency-Key must be 1-128 ASCII printable characters"),
            "got: {err:?}"
        );
    }

    #[test]
    fn key_with_internal_whitespace_is_err() {
        let headers = headers_with_key("abc 123");
        let err = extract_idempotency_key(&headers).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "Idempotency-Key must be 1-128 ASCII printable characters"),
            "got: {err:?}"
        );
    }

    #[test]
    fn non_utf8_bytes_is_err() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "idempotency-key",
            HeaderValue::from_bytes(&[0xFF, 0xFE, 0xFD]).expect("raw bytes are a legal HeaderValue"),
        );
        let err = extract_idempotency_key(&headers).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "Idempotency-Key must be 1-128 ASCII printable characters"),
            "got: {err:?}"
        );
    }
}
