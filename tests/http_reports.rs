//! HTTP integration tests for the reports module's endpoints:
//! `GET /reports/admin`, `GET /reports/coach`, `GET /reports/me`,
//! `GET /reports/admin/activity`.

mod common;

use common::fixtures::seed_coach;
use common::http::spawn_test_app;
use sqlx::PgPool;

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_report_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/reports/admin").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn admin_report_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("reports-admin-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/reports/admin")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_report_as_admin_returns_200_with_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/reports/admin")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["revenue"]["trend"].as_array().expect("trend array").len(), 12);
    assert!(body["revenue"]["this_month_cents"].is_number());
    assert!(body["revenue"]["last_month_cents"].is_number());
    assert!(body["members"]["total"].is_number());
    assert!(body["members"]["new_this_month"].is_number());
    assert!(body["members"]["active"].is_number());
    assert!(body["courses"].as_array().is_some());
    assert!(body["coaches"].as_array().is_some());

    // Round 4 Phase 4 金流 sections — zero-filled on an empty DB, never 500.
    assert!(body["kpis"]["new_members"]["this_month"].is_number());
    assert!(body["kpis"]["new_enrolments"]["last_month"].is_number());
    assert!(body["kpis"]["paid_orders_count"]["this_month"].is_number());
    assert!(body["kpis"]["attendance_rate"]["this_month"].is_null(), "no-data month is null");
    assert_eq!(body["revenue_breakdown"].as_array().expect("breakdown array").len(), 6);
    assert_eq!(body["income_sources_12m"].as_array().expect("12m array").len(), 72);
    assert_eq!(body["category_split"].as_array().expect("split array").len(), 5);
    assert_eq!(body["payment_split"].as_array().expect("payment array").len(), 0);

    // Round 4 Phase 4 人流 sections — fixed-bucket zero-fills on an empty DB.
    assert_eq!(body["attendance_distribution"].as_array().expect("attDist array").len(), 4);
    assert_eq!(body["age_distribution"].as_array().expect("ageDist array").len(), 6);
    assert_eq!(body["tier_distribution"].as_array().expect("tierDist array").len(), 4);
    assert_eq!(body["retention"].as_array().expect("retention array").len(), 6);
    assert!(body["funnel"]["trial_inquiries"].is_number());
    assert!(body["funnel"]["new_enrolments"].is_number());
    assert_eq!(body["weekday_load"].as_array().expect("weekday array").len(), 7);
    assert_eq!(body["venue_usage"].as_array().expect("venue array").len(), 0);
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn coach_report_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/reports/coach").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn coach_report_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("reports-coach-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/reports/coach")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn coach_report_as_admin_without_coach_role_returns_403(db: PgPool) {
    // Task brief specifies `require_role("coach")`, not `require_any_role`
    // — an admin who does not also hold the coach role is deliberately
    // forbidden here, unlike some other coach-domain endpoints (e.g.
    // `GET /sessions/today`) that accept either role.
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/reports/coach")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn coach_report_role_but_no_coach_row_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_user_id, token) =
        app.seed_user_with_roles("reports-coach-no-row@example.com", &["coach"]).await;

    let resp = app
        .get("/api/v1/reports/coach")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn coach_report_as_coach_returns_200_with_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (user_id, token) =
        app.seed_user_with_roles("reports-coach-ok@example.com", &["coach"]).await;
    seed_coach(&app.db, user_id, "Report Coach").await;

    let resp = app
        .get("/api/v1/reports/coach")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["today_sessions"].is_number());
    assert!(body["pending_attendance"].is_number());
    assert!(body["unread_messages"].is_number());
    assert!(body["student_count"].is_number());
    assert!(body["attendance_rate_30d"].is_null(), "no attendance data yet -> null");
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn member_report_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/reports/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn member_report_as_member_returns_200_with_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("reports-me-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/reports/me")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["attended_total"].is_number());
    assert!(body["attendance_rate"].is_null());
    assert!(body["points_balance"].is_number());
    assert!(body["active_enrolments"].is_number());
    assert!(body["upcoming_sessions_7d"].is_number());
}

#[sqlx::test]
async fn member_report_as_coach_role_also_returns_200(db: PgPool) {
    // "登入即可" (any authenticated user) — no role restriction.
    let app = spawn_test_app(db).await;
    let (_user_id, token) =
        app.seed_user_with_roles("reports-me-coach@example.com", &["coach"]).await;

    let resp = app
        .get("/api/v1/reports/me")
        .authorization_bearer(&token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// GET /reports/admin/activity
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_activity_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/reports/admin/activity").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn admin_activity_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("reports-activity-member@example.com", "Password!234").await;

    let resp = app
        .get("/api/v1/reports/admin/activity")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn admin_activity_as_admin_returns_200_with_shape(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .get("/api/v1/reports/admin/activity")
        .authorization_bearer(&admin_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["items"].as_array().is_some());
}
