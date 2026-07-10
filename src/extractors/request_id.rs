use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

/// The `x-request-id` header value on the incoming request, if present.
///
/// `SetRequestIdLayer` (see `startup::build_router`) stamps this header on
/// every request before it reaches the router — reusing the caller's own
/// value when the client already sent one (the layer never overwrites an
/// existing header), or minting a fresh UUID otherwise. Handlers extract it
/// here and thread it into `outbox::insert_domain_event_tx` as
/// `correlation_id`, so a published Kafka event can be traced back to the
/// HTTP request that caused it.
///
/// Extraction never fails — a request without the header (impossible in
/// production since the layer always sets one, but easy to hit in a test
/// that bypasses the layer) simply yields `RequestId(None)`.
pub struct RequestId(pub Option<String>);

impl<S> FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let id = parts
            .headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        Ok(RequestId(id))
    }
}
