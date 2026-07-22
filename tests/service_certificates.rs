//! Service-layer tests for `modules::certificates::service`'s two create
//! paths — pinned around the C4 pool→tx refactor (task brief).
//!
//! `tests/http_certificates.rs` already covers the wire-level 401/404/409/422
//! branches end to end and stays untouched by this task; this file adds:
//! - a notification-landing assertion `service::` can make directly without
//!   an HTTP round trip,
//! - a zero-side-effect assertion on a rejected authorization,
//! - the one branch HTTP can't reach at all: a `coach`-role caller with no
//!   `coaches` row (`coaches_service::resolve` returns `None`), and
//! - a rollback assertion on the report-card duplicate-409 path (exactly one
//!   row survives, not a partial second row from the aborted insert).
//!
//! All 7 tests pin behavior that already holds at HEAD — the refactor changes
//! the create paths' internal write shape (pool → transaction), not their
//! observable behavior.

mod common;

use chrono::{NaiveDate, Utc};
use sqlx::PgPool;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::certificates::dto::{CreateCertificateRequest, CreateReportCardRequest};
use dream_fly_backend::modules::certificates::service;

use common::fixtures::{seed_coach, seed_course, seed_enrolment};
use common::{admin_auth, coach_auth, seed_member};

fn issued_on() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 7, 1).unwrap()
}

// ---------------------------------------------------------------------------
// create_certificate
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_certificate_by_owning_coach_succeeds_and_notifies(db: PgPool) {
    let coach_user_id = seed_member(&db, "cert-svc-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user_id, "Cert Svc Coach").await;
    let course_id = seed_course(&db, "Cert Svc Course", Some(coach_id)).await;
    let member_id = seed_member(&db, "cert-svc-student@example.com", "Password!234").await;
    seed_enrolment(&db, member_id, course_id, "active", Utc::now()).await;

    let auth = coach_auth(coach_user_id);
    let req = CreateCertificateRequest {
        user_id: member_id,
        course_id: Some(course_id),
        title: "體操初級證書".to_string(),
        level: Some("初級".to_string()),
        issued_on: issued_on(),
        note: Some("表現優異".to_string()),
    };

    let resp = service::create_certificate(&db, &auth, req).await.unwrap();
    assert_eq!(resp.course_id, Some(course_id));
    assert_eq!(resp.course_name.as_deref(), Some("Cert Svc Course"));
    assert_eq!(resp.title, "體操初級證書");
    assert_eq!(resp.level.as_deref(), Some("初級"));

    // The "you got a new certificate" notification lands after the write
    // transaction commits.
    let (title, message) = common::latest_notification(&db, member_id, "system")
        .await
        .expect("certificate-issued notification row");
    assert_eq!(title, "新證書");
    assert_eq!(message, "你獲得了新證書：體操初級證書");
}

#[sqlx::test]
async fn create_certificate_for_cancelled_enrolment_student_succeeds(db: PgPool) {
    // Historical students (cancelled enrolment) can still be certified —
    // contract §3.22 explicit semantics; `user_has_enrolment_with_coach`
    // doesn't filter by enrolment status.
    let coach_user_id = seed_member(&db, "cert-svc-hist-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user_id, "Cert Svc Hist Coach").await;
    let course_id = seed_course(&db, "Cert Svc Hist Course", Some(coach_id)).await;
    let member_id = seed_member(&db, "cert-svc-hist-student@example.com", "Password!234").await;
    seed_enrolment(&db, member_id, course_id, "cancelled", Utc::now()).await;

    let auth = coach_auth(coach_user_id);
    let req = CreateCertificateRequest {
        user_id: member_id,
        course_id: None,
        title: "結業證書".to_string(),
        level: None,
        issued_on: issued_on(),
        note: None,
    };

    let resp = service::create_certificate(&db, &auth, req).await.unwrap();
    assert_eq!(resp.title, "結業證書");
}

#[sqlx::test]
async fn create_certificate_by_non_owning_coach_returns_403_with_no_side_effects(db: PgPool) {
    let coach_user_id = seed_member(&db, "cert-svc-other-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user_id, "Cert Svc Other Coach").await;
    seed_course(&db, "Cert Svc Other Course", Some(coach_id)).await;

    // Never enrolled in any course taught by this coach.
    let member_id = seed_member(&db, "cert-svc-cross-student@example.com", "Password!234").await;

    let auth = coach_auth(coach_user_id);
    let req = CreateCertificateRequest {
        user_id: member_id,
        course_id: None,
        title: "體操初級證書".to_string(),
        level: None,
        issued_on: issued_on(),
        note: None,
    };

    let err = service::create_certificate(&db, &auth, req).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)));

    let cert_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM certificates")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(cert_count, 0, "a rejected authorization must not insert a certificate row");

    let notif_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(notif_count, 0, "a rejected authorization must not send a notification");
}

#[sqlx::test]
async fn create_certificate_by_coach_without_coach_row_returns_403(db: PgPool) {
    // A caller with the `coach` role but no row in `coaches` at all —
    // `coaches_service::resolve` returns `None` and `create_certificate`
    // 403s before `user_has_enrolment_with_coach` is ever reached. HTTP
    // tests always pair a coach token with a `seed_coach` row, so
    // `http_certificates.rs` never exercises this branch.
    let coach_user_id =
        seed_member(&db, "cert-svc-no-coach-row@example.com", "Password!234").await;
    let member_id =
        seed_member(&db, "cert-svc-no-coach-row-target@example.com", "Password!234").await;

    let auth = coach_auth(coach_user_id);
    let req = CreateCertificateRequest {
        user_id: member_id,
        course_id: None,
        title: "體操初級證書".to_string(),
        level: None,
        issued_on: issued_on(),
        note: None,
    };

    let err = service::create_certificate(&db, &auth, req).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)));
}

#[sqlx::test]
async fn create_certificate_by_admin_succeeds_without_course(db: PgPool) {
    let admin_user_id = seed_member(&db, "cert-svc-admin@example.com", "Password!234").await;
    let member_id = seed_member(&db, "cert-svc-admin-target@example.com", "Password!234").await;

    let auth = admin_auth(admin_user_id);
    let req = CreateCertificateRequest {
        user_id: member_id,
        course_id: None,
        title: "體操初級證書".to_string(),
        level: None,
        issued_on: issued_on(),
        note: None,
    };

    let resp = service::create_certificate(&db, &auth, req).await.unwrap();
    assert_eq!(resp.title, "體操初級證書");
    assert!(resp.course_id.is_none());
    assert!(resp.course_name.is_none());
}

// ---------------------------------------------------------------------------
// create_report_card
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn create_report_card_by_owning_coach_succeeds(db: PgPool) {
    let coach_user_id = seed_member(&db, "rc-svc-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user_id, "RC Svc Coach").await;
    let course_id = seed_course(&db, "RC Svc Course", Some(coach_id)).await;
    let member_id = seed_member(&db, "rc-svc-student@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member_id, course_id, "active", Utc::now()).await;

    let auth = coach_auth(coach_user_id);
    let req = CreateReportCardRequest {
        enrolment_id,
        term_label: "2026 Spring".to_string(),
        comment: Some("進步很多".to_string()),
        rating: Some(5),
    };

    let resp = service::create_report_card(&db, &auth, req).await.unwrap();
    assert_eq!(resp.course_id, course_id);
    assert_eq!(resp.course_name, "RC Svc Course");
    assert_eq!(resp.term_label, "2026 Spring");
    assert_eq!(resp.rating, Some(5));

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM report_cards WHERE enrolment_id = $1")
            .bind(enrolment_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn create_report_card_duplicate_term_returns_409_leaves_one_row(db: PgPool) {
    let coach_user_id = seed_member(&db, "rc-svc-dup-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user_id, "RC Svc Dup Coach").await;
    let course_id = seed_course(&db, "RC Svc Dup Course", Some(coach_id)).await;
    let member_id = seed_member(&db, "rc-svc-dup-student@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member_id, course_id, "active", Utc::now()).await;

    let auth = coach_auth(coach_user_id);
    let first = CreateReportCardRequest {
        enrolment_id,
        term_label: "2026 Spring".to_string(),
        comment: None,
        rating: None,
    };
    service::create_report_card(&db, &auth, first).await.unwrap();

    let second = CreateReportCardRequest {
        enrolment_id,
        term_label: "2026 Spring".to_string(),
        comment: None,
        rating: None,
    };
    let err = service::create_report_card(&db, &auth, second).await.unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));

    // The rejected duplicate must roll back cleanly — exactly the first row
    // survives, not a partial second row left behind by the aborted insert.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM report_cards WHERE enrolment_id = $1")
            .bind(enrolment_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(count, 1, "duplicate term_label must roll back with zero extra rows");
}
