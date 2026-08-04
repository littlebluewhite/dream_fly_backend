//! Regression net for the three-tier route_layer authorization surface
//! (`admin_api`/`staff_api`/`coach_api` in `src/startup.rs`) plus the
//! `LoginRequired` extractor (`src/extractors/auth.rs`).
//!
//! Each gate is exercised through one representative endpoint rather than
//! every moved handler — per-handler role coverage for each site already
//! lives in that module's own `tests/http_*.rs`. This file's job is to pin
//! the *shape* of each gate (no token / wrong role / right role) in one
//! place, independent of which handler happens to sit behind it.
//!
//! `tests/middleware_auth_extractor.rs` covers the admin/staff gates from
//! the extractor's point of view (malformed/expired tokens, deactivated
//! users); this file focuses on the route_layer's role decision plus the
//! `LoginRequired`/deleted-`_auth` distinction instead.

mod common;

use common::http::spawn_test_app;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Admin gate — representative endpoint `GET /api/v1/users` (admin_router(),
// `require_admin` route_layer).
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_gate_no_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/users").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn admin_gate_member_token_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("gate-admin-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_gate_admin_token_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/users")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// Staff gate — representative endpoint `GET /api/v1/sessions/today`
// (staff_router(), `require_staff` route_layer: admin OR coach).
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn staff_gate_no_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/sessions/today").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn staff_gate_member_token_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("gate-staff-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/sessions/today")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn staff_gate_coach_token_returns_200(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_coach_user_id, coach_token) =
        app.seed_user_with_roles("gate-staff-coach@example.com", &["coach"]).await;

    let resp = app
        .get("/api/v1/sessions/today")
        .authorization_bearer(&coach_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// Coach gate asymmetry — `GET /api/v1/reports/coach` (coach_router(),
// `require_coach` route_layer: coach role only, deliberately no admin
// bypass). Canonical home for this invariant; the original
// `coach_report_as_admin_without_coach_role_returns_403` test in
// `tests/http_reports.rs` stays put, unmodified.
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn coach_gate_admin_without_coach_role_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/reports/coach")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// `LoginRequired` — "logged in, no role check" gate on endpoints outside any
// admin/staff/coach router. Two representative sites (see the
// `LoginRequired` doc comment in `src/extractors/auth.rs`): a member token
// must NOT get 401 here, unlike the role-gated endpoints above.
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn login_required_coupons_validate_no_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/coupons/GATENOPE1/validate").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn login_required_coupons_validate_member_token_returns_non_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("gate-login-coupons@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/coupons/GATENOPE1/validate")
        .authorization_bearer(&user.access_token)
        .await;
    assert_ne!(resp.status_code(), 401, "body={}", resp.text());
}

#[sqlx::test]
async fn login_required_course_sessions_no_token_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .get(&format!("/api/v1/courses/{}/sessions", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn login_required_course_sessions_member_token_returns_non_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("gate-login-sessions@example.com", "Password!234").await;

    let resp = app
        .get(&format!("/api/v1/courses/{}/sessions", Uuid::now_v7()))
        .authorization_bearer(&user.access_token)
        .await;
    assert_ne!(resp.status_code(), 401, "body={}", resp.text());
}
