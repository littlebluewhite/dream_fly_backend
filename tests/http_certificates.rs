//! HTTP integration tests for the certificates module's endpoints:
//! `POST /report-cards`, `GET /report-cards/me`, `POST /certificates`,
//! `GET /certificates/me`.

mod common;

use chrono::Utc;
use common::fixtures::{seed_coach, seed_course, seed_enrolment};
use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// POST /report-cards
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_report_card_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/report-cards")
        .json(&json!({"enrolment_id": Uuid::now_v7(), "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_report_card_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("rc-member@example.com", "Password!234").await;
    let course_id = seed_course(&app.db, "RC Member Course", None).await;
    let enrolment_id = seed_enrolment(&app.db, user.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&user.access_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_by_owning_coach_succeeds(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("rc-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "RC Coach").await;
    let course_id = seed_course(&app.db, "RC Course", Some(coach_id)).await;
    let member = app.register_member("rc-student@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&coach_token)
        .json(&json!({
            "enrolment_id": enrolment_id,
            "term_label": "2026 Spring",
            "comment": "進步很多",
            "rating": 5
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["course_id"], course_id.to_string());
    assert_eq!(body["course_name"], "RC Course");
    assert_eq!(body["term_label"], "2026 Spring");
    assert_eq!(body["comment"], "進步很多");
    assert_eq!(body["rating"], 5);
    assert_eq!(body["created_by_name"], "Seeded User");
    assert!(body["id"].as_str().is_some());
}

#[sqlx::test]
async fn create_report_card_by_non_owning_coach_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (other_coach_user, other_coach_token) =
        app.seed_user_with_roles("rc-other-coach@example.com", &["coach"]).await;
    seed_coach(&app.db, other_coach_user, "RC Other Coach").await;

    // Course has no coach assigned at all (distinct from `other_coach`).
    let course_id = seed_course(&app.db, "RC Unowned Course", None).await;
    let member = app.register_member("rc-student2@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&other_coach_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_by_a_different_coachs_course_returns_403(db: PgPool) {
    // Distinct from the test above: here the course DOES belong to a coach —
    // just not the caller — exercising the `coach.id == course_coach_id`
    // comparison branch rather than the `course_coach_id.is_none()` fallback.
    let app = spawn_test_app(db).await;
    let (coach_a_user, _coach_a_token) =
        app.seed_user_with_roles("rc-coach-a@example.com", &["coach"]).await;
    let coach_a_id = seed_coach(&app.db, coach_a_user, "RC Coach A").await;
    let course_id = seed_course(&app.db, "RC Coach A Course", Some(coach_a_id)).await;

    let (coach_b_user, coach_b_token) =
        app.seed_user_with_roles("rc-coach-b@example.com", &["coach"]).await;
    seed_coach(&app.db, coach_b_user, "RC Coach B").await;

    let member = app.register_member("rc-student3@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&coach_b_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_by_admin_bypasses_ownership(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "RC Admin Course", None).await;
    let member = app.register_member("rc-admin-student@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_unknown_enrolment_returns_404(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": Uuid::now_v7(), "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 404, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_duplicate_term_returns_409(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "RC Dup Course", None).await;
    let member = app.register_member("rc-dup-student@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let first = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(first.status_code(), 200, "body={}", first.text());

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring"}))
        .await;
    assert_eq!(resp.status_code(), 409, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_rating_zero_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "RC Rating Low Course", None).await;
    let member = app.register_member("rc-rating-low@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring", "rating": 0}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

#[sqlx::test]
async fn create_report_card_rating_six_returns_422(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "RC Rating High Course", None).await;
    let member = app.register_member("rc-rating-high@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/report-cards")
        .authorization_bearer(&admin_token)
        .json(&json!({"enrolment_id": enrolment_id, "term_label": "2026 Spring", "rating": 6}))
        .await;
    assert_eq!(resp.status_code(), 422, "body={}", resp.text());
}

// ---------------------------------------------------------------------------
// GET /report-cards/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn my_report_cards_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/report-cards/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn my_report_cards_only_shows_own(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let course_id = seed_course(&app.db, "RC Me Course", None).await;
    let member_a = app.register_member("rc-me-a@example.com", "Password!234").await;
    let member_b = app.register_member("rc-me-b@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&app.db, member_a.user_id, course_id, "active", Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&app.db, member_b.user_id, course_id, "active", Utc::now()).await;

    for (enrolment_id, term) in [(enrolment_a, "2026 Spring A"), (enrolment_b, "2026 Spring B")] {
        let resp = app
            .post("/api/v1/report-cards")
            .authorization_bearer(&admin_token)
            .json(&json!({"enrolment_id": enrolment_id, "term_label": term}))
            .await;
        assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    }

    let resp = app
        .get("/api/v1/report-cards/me")
        .authorization_bearer(&member_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 1, "member must only see their own report card");
    assert_eq!(arr[0]["term_label"], "2026 Spring A");
}

// ---------------------------------------------------------------------------
// POST /certificates
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_certificate_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .post("/api/v1/certificates")
        .json(&json!({
            "user_id": Uuid::now_v7(), "title": "體操初級證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn create_certificate_as_member_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("cert-member@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&user.access_token)
        .json(&json!({
            "user_id": user.user_id, "title": "體操初級證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_certificate_for_own_active_student_succeeds_and_notifies(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("cert-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Cert Coach").await;
    let course_id = seed_course(&app.db, "Cert Course", Some(coach_id)).await;
    let member = app.register_member("cert-student@example.com", "Password!234").await;
    seed_enrolment(&app.db, member.user_id, course_id, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&coach_token)
        .json(&json!({
            "user_id": member.user_id,
            "course_id": course_id,
            "title": "體操初級證書",
            "level": "初級",
            "issued_on": "2026-07-01",
            "note": "表現優異"
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert_eq!(body["course_id"], course_id.to_string());
    assert_eq!(body["course_name"], "Cert Course");
    assert_eq!(body["title"], "體操初級證書");
    assert_eq!(body["level"], "初級");
    assert_eq!(body["issued_on"], "2026-07-01");
    assert_eq!(body["note"], "表現優異");
    assert!(body["id"].as_str().is_some());

    let message: String = sqlx::query_scalar(
        "SELECT message FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(member.user_id)
    .fetch_one(&app.db)
    .await
    .expect("fetch notification");
    assert_eq!(message, "你獲得了新證書：體操初級證書");
}

#[sqlx::test]
async fn create_certificate_for_cancelled_enrolment_student_succeeds(db: PgPool) {
    // Historical students (cancelled enrolment) can still be certified —
    // contract §3.22 explicit semantics.
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("cert-hist-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Cert Hist Coach").await;
    let course_id = seed_course(&app.db, "Cert Hist Course", Some(coach_id)).await;
    let member = app.register_member("cert-hist-student@example.com", "Password!234").await;
    seed_enrolment(&app.db, member.user_id, course_id, "cancelled", Utc::now()).await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&coach_token)
        .json(&json!({
            "user_id": member.user_id, "title": "結業證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
}

#[sqlx::test]
async fn create_certificate_for_non_student_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_user_id, coach_token) =
        app.seed_user_with_roles("cert-cross-coach@example.com", &["coach"]).await;
    let coach_id = seed_coach(&app.db, coach_user_id, "Cert Cross Coach").await;
    seed_course(&app.db, "Cert Cross Coach Course", Some(coach_id)).await;

    // This member has never enrolled in any of the coach's courses.
    let member = app.register_member("cert-cross-student@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&coach_token)
        .json(&json!({
            "user_id": member.user_id, "title": "體操初級證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_certificate_for_other_coachs_student_returns_403(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (coach_a_user, coach_a_token) =
        app.seed_user_with_roles("cert-coach-a@example.com", &["coach"]).await;
    seed_coach(&app.db, coach_a_user, "Cert Coach A").await;
    let coach_b_user =
        common::seed_member(&app.db, "cert-coach-b@example.com", "Password!234").await;
    let coach_b_id = seed_coach(&app.db, coach_b_user, "Cert Coach B").await;
    let course_b = seed_course(&app.db, "Cert Coach B Course", Some(coach_b_id)).await;

    let member = app.register_member("cert-coach-b-student@example.com", "Password!234").await;
    seed_enrolment(&app.db, member.user_id, course_b, "active", Utc::now()).await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&coach_a_token)
        .json(&json!({
            "user_id": member.user_id, "title": "體操初級證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 403, "body={}", resp.text());
}

#[sqlx::test]
async fn create_certificate_by_admin_bypasses_ownership(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let member = app.register_member("cert-admin-student@example.com", "Password!234").await;

    let resp = app
        .post("/api/v1/certificates")
        .authorization_bearer(&admin_token)
        .json(&json!({
            "user_id": member.user_id, "title": "體操初級證書", "issued_on": "2026-07-01"
        }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    assert!(body["course_id"].is_null());
    assert!(body["course_name"].is_null());
}

// ---------------------------------------------------------------------------
// GET /certificates/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn my_certificates_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/certificates/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn my_certificates_only_shows_own(db: PgPool) {
    let app = spawn_test_app(db).await;
    let (_admin_id, admin_token) = app.seed_admin().await;
    let member_a = app.register_member("cert-me-a@example.com", "Password!234").await;
    let member_b = app.register_member("cert-me-b@example.com", "Password!234").await;

    for (user, title) in [(&member_a, "證書 A"), (&member_b, "證書 B")] {
        let resp = app
            .post("/api/v1/certificates")
            .authorization_bearer(&admin_token)
            .json(&json!({"user_id": user.user_id, "title": title, "issued_on": "2026-07-01"}))
            .await;
        assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    }

    let resp = app
        .get("/api/v1/certificates/me")
        .authorization_bearer(&member_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().expect("plain array, not an envelope");
    assert_eq!(arr.len(), 1, "member must only see their own certificate");
    assert_eq!(arr[0]["title"], "證書 A");
}
