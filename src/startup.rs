use std::time::Duration;

use axum::{
    Json, Router,
    extract::Request,
    http::StatusCode,
    middleware,
    routing::get,
};
use serde_json::{Value, json};
use tower_http::{
    compression::CompressionLayer,
    limit::RequestBodyLimitLayer,
    propagate_header::PropagateHeaderLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::middleware::cors::cors_layer;
use crate::middleware::rate_limit::rate_limit_middleware;
use crate::modules;
use crate::state::AppState;

async fn health_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> (StatusCode, Json<Value>) {
    // Bound each dependency probe so a wedged Redis/PG cannot hang liveness.
    let db_ok = tokio::time::timeout(
        Duration::from_millis(500),
        sqlx::query("SELECT 1").execute(&state.db),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some();

    let redis_ok = tokio::time::timeout(
        Duration::from_millis(500),
        redis::cmd("PING").query_async::<String>(&mut state.redis.clone()),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some();

    let kafka_status = if state.kafka_producer.is_some() {
        "connected"
    } else {
        "disabled"
    };

    let healthy = db_ok && redis_ok;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "status": if healthy { "healthy" } else { "degraded" },
            "services": {
                "database": if db_ok { "up" } else { "down" },
                "redis": if redis_ok { "up" } else { "down" },
                "kafka": kafka_status,
            }
        })),
    )
}

pub fn build_router(state: AppState) -> Router {
    use axum::http::{HeaderName, HeaderValue};

    let cors = cors_layer(&state.config.server);

    let api_v1 = Router::new()
        .route("/health", get(health_check))
        .merge(modules::auth::routes::router())
        .merge(modules::users::routes::router())
        .merge(modules::permissions::routes::router())
        .merge(modules::coaches::routes::router())
        .merge(modules::courses::routes::router())
        .merge(modules::venues::routes::router())
        .merge(modules::schedule::routes::router())
        .merge(modules::bookings::routes::router())
        .merge(modules::products::routes::router())
        .merge(modules::cart::routes::router())
        .merge(modules::orders::routes::router())
        .merge(modules::posts::routes::router())
        .merge(modules::notifications::routes::router())
        .merge(modules::contact::routes::router())
        .merge(modules::coupons::routes::router())
        .merge(modules::subscriptions::routes::router())
        .merge(modules::enrolments::routes::router())
        .merge(modules::waitlist::routes::router())
        .merge(modules::points::routes::router());

    // Basic security headers. The API is JSON-only so CSP isn't critical, but
    // sniffing/referrer leaks and clickjacking protection are cheap to add.
    let security_headers = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("strict-transport-security"),
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ));

    let request_id_header = HeaderName::from_static("x-request-id");

    // Build each request's tracing span WITH the `x-request-id` header as a
    // span field so every log emitted inside the request (including from
    // handler code) can be correlated back to the request. The header is
    // set by `SetRequestIdLayer` which runs before this, so by the time
    // `make_span` fires the id is present in `req.headers()`.
    let trace_layer = TraceLayer::new_for_http().make_span_with(|req: &Request| {
        let request_id = req
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        tracing::info_span!(
            "http_request",
            method = %req.method(),
            uri = %req.uri(),
            request_id = %request_id,
        )
    });

    Router::new()
        .nest("/api/v1", api_v1)
        // Layer ordering: `.layer(X)` wraps everything that comes BEFORE it,
        // so the LAST layer listed is the OUTERMOST at runtime.
        //
        // Runtime order (outermost → innermost):
        //   1. CORS                      — reject disallowed origins first
        //   2. Request ID (set + propagate) — attach X-Request-Id to every span
        //   3. TraceLayer                — one span per request, including CORS
        //                                  rejects and request-id is in scope
        //   4. Security headers          — added on every response
        //   5. Rate limit                — throttle before any heavy work
        //   6. Body limit                — 2MB cap before we read the body
        //   7. Compression               — response compression
        //   8. Handler                   — business logic
        .layer(CompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_middleware,
        ))
        .layer(security_headers)
        .layer(trace_layer)
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(PropagateHeaderLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(
            request_id_header,
            MakeRequestUuid,
        ))
        .layer(cors)
        .with_state(state)
}
