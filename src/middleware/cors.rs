use axum::http::{HeaderName, HeaderValue, Method};
use tower_http::cors::{Any, CorsLayer};

use crate::config::ServerConfig;

/// Build the CORS layer. When `allowed_origins` is empty (development) we
/// fall back to `Any` for convenience; production rejects this at startup via
/// `validate_production_config`, so an empty list can never reach prod.
///
/// Note: we use an explicit method + header allowlist instead of `Any` for
/// headers, because `allow_headers(Any)` silently weakens the protection
/// CORS provides against hostile origins reading JSON replies.
pub fn cors_layer(config: &ServerConfig) -> CorsLayer {
    let allowed_methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];

    let allowed_headers: Vec<HeaderName> = vec![
        HeaderName::from_static("authorization"),
        HeaderName::from_static("content-type"),
        HeaderName::from_static("accept"),
        HeaderName::from_static("x-request-id"),
        HeaderName::from_static("idempotency-key"),
    ];

    let cors = CorsLayer::new()
        .allow_methods(allowed_methods)
        .allow_headers(allowed_headers)
        .max_age(std::time::Duration::from_secs(600));

    if config.allowed_origins.is_empty() {
        // Dev-only fallback. `validate_production_config` rejects this at
        // startup when APP_ENV=production.
        cors.allow_origin(Any)
    } else {
        let origins: Vec<HeaderValue> = config
            .allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        cors.allow_origin(origins)
    }
}
