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
use crate::middleware::rate_limit::{rate_limit_middleware, strict_rate_limit};
use crate::middleware::require_admin::require_admin;
use crate::middleware::require_coach::require_coach;
use crate::middleware::require_staff::require_staff;
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

    // Admin 半邊:17 個模組的 `admin_router()` 合併後,單點掛上 `require_admin`
    // route_layer——admin 授權從 34 份 handler 首行儀式收斂為此一層。route_layer
    // 只包住 admin 方法(及其 per-path 405 fallback),與公開 router 帶入同路徑的
    // sibling 方法互不影響(共用路徑按 method 拆;見 `middleware::require_admin`
    // 檔頭與各模組 `admin_router()` 註解)。permissions/settings 全數 admin,公開
    // 半邊已無 `router()`,只在此出現。
    let admin_api = Router::new()
        .merge(modules::settings::routes::admin_router())
        .merge(modules::permissions::routes::admin_router())
        .merge(modules::contact::routes::admin_router())
        .merge(modules::schedule::routes::admin_router())
        .merge(modules::coupons::routes::admin_router())
        .merge(modules::users::routes::admin_router())
        .merge(modules::reports::routes::admin_router())
        .merge(modules::orders::routes::admin_router())
        .merge(modules::waitlist::routes::admin_router())
        .merge(modules::bookings::routes::admin_router())
        .merge(modules::venues::routes::admin_router())
        .merge(modules::products::routes::admin_router())
        .merge(modules::courses::routes::admin_router())
        .merge(modules::coaches::routes::admin_router())
        .merge(modules::rewards::routes::admin_router())
        .merge(modules::posts::routes::admin_router())
        .merge(modules::points::routes::admin_router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    // Staff 半邊:6 個模組的 `staff_router()` 合併後,單點掛上 `require_staff`
    // route_layer——coach 層級(admin 或 coach)授權從 ~9 份 handler 首行儀式
    // 收斂為此一層,設計對稱於上方的 admin_api。Request-data-dependent 的細
    // 粒度檢查(`require_course_coach`、`is_admin()` 分支)不在此列,留在
    // service。僅剩一個例外原地保留、不併入此閘門(該 handler 已加註解):
    // `rewards::list` 的條件式 `?all=true` 閘門依賴 query 參數,本質不可
    // 上移。coach-only carve-out(`attendance::my_students`、
    // `reports::coach_report`)已獨立收斂至下方的 `coach_api`。
    let staff_api = Router::new()
        .merge(modules::leave::routes::staff_router())
        .merge(modules::sessions::routes::staff_router())
        .merge(modules::attendance::routes::staff_router())
        .merge(modules::certificates::routes::staff_router())
        .merge(modules::subscriptions::routes::staff_router())
        .merge(modules::posts::routes::staff_router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_staff));

    // Coach 半邊:兩個模組的 `coach_router()` 合併後,單點掛上 `require_coach`
    // route_layer,設計對稱於上方的 admin_api/staff_api——差異僅角色集合:
    // `require_coach` 不含 admin bypass(兩條路徑語意皆為 admin 刻意排除,
    // 見 `middleware::require_coach` 檔頭)。這是 admin/staff/coach 三層
    // route_layer 家族補齊的第三個 gate,反轉了先前「第三個單角色 gate 不
    // 值得」的決定——現況兩條路徑同形,route-table 可信度已判定值得。
    let coach_api = Router::new()
        .merge(modules::attendance::routes::coach_router())
        .merge(modules::reports::routes::coach_router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_coach));

    let api_v1 = Router::new()
        .route("/health", get(health_check))
        // auth 的嚴格桶(10/min)透過 route_layer 掛在 throttled_router() 上,
        // 宣告形狀比照下方 admin_api/staff_api;`/auth/logout` 不吃嚴格桶,
        // 走旁邊的 router() merge(見 `middleware::rate_limit::strict_rate_limit`)。
        .merge(
            Router::new()
                .merge(modules::auth::routes::throttled_router())
                .route_layer(middleware::from_fn_with_state(state.clone(), strict_rate_limit)),
        )
        .merge(modules::auth::routes::router())
        .merge(modules::users::routes::router())
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
        .merge(modules::leave::routes::router())
        .merge(modules::messages::routes::router())
        .merge(modules::certificates::routes::router())
        .merge(modules::sessions::routes::router())
        .merge(modules::waitlist::routes::router())
        .merge(modules::points::routes::router())
        .merge(modules::rewards::routes::router())
        .merge(modules::reports::routes::router())
        .merge(admin_api)
        .merge(staff_api)
        .merge(coach_api);

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
