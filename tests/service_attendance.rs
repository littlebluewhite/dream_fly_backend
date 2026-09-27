//! Repository-level tests for `attendance::repository::upsert_attendance_tx`'s
//! self-defending 核准恆勝 guard (ADR-0008). These call the repository directly
//! (no HTTP, no `marking::plan` pre-check) so they exercise the `ON CONFLICT
//! ... WHERE` clause in isolation — the deterministic, single-connection proxy
//! for the TOCTOU race the guard closes. They assert both the *row state*
//! after the upsert and the `rows_affected` signal: the blocked case affects
//! zero rows without erroring, and zero has no other producer — which is what
//! lets `attendance::service` convert an in-window block into the same
//! batch-wide 422 the pre-check gives.

mod common;

use std::sync::Arc;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::attendance::dto::AttendanceRecordEntry;
use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::attendance::repository;
use dream_fly_backend::modules::attendance::service as attendance_service;
use dream_fly_backend::modules::leave::service as leave_service;

use common::fixtures::{seed_attendance, seed_course, seed_course_session, seed_enrolment, seed_leave_request};
use common::{admin_auth, seed_member, studio_now_utc};

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn yesterday() -> chrono::NaiveDate {
    (Utc::now() - Duration::days(1)).date_naive()
}

/// Guard blocks the write: an `approved` leave, already projected to an
/// attendance `leave` row, cannot be overwritten present by a direct upsert.
/// All three OR branches of the guard are false — `EXCLUDED.status = 'present'`
/// (not leave), the existing row *is* `leave`, and an `approved` leave request
/// EXISTS — so zero rows change and the row stays `leave`. This is the
/// deterministic proxy for the concurrency window `marking::plan`'s pre-check
/// alone can't close.
#[sqlx::test]
async fn upsert_guard_blocks_present_over_approved_leave(db: PgPool) {
    let course_id = seed_course(&db, "Guard Blocked Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = seed_member(&db, "att-guard-blocked@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member, course_id, "active", Utc::now()).await;
    seed_leave_request(&db, enrolment_id, session_id, "approved").await;
    seed_attendance(&db, session_id, enrolment_id, "leave", member).await;

    let mut tx = db.begin().await.expect("begin");
    let affected = repository::upsert_attendance_tx(
        &mut tx,
        session_id,
        enrolment_id,
        AttendanceStatus::Present,
        member,
    )
    .await
    .expect("upsert must not error even when the guard blocks the update");
    tx.commit().await.expect("commit");

    assert_eq!(
        affected, 0,
        "a blocked upsert must report zero rows — the signal `attendance::service` \
         converts into the batch-wide 422 inside the race window"
    );

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&db)
    .await
    .expect("fetch status");
    assert_eq!(status, "leave", "the guard must keep the approved-leave row as leave");
}

/// Guard allows the write: a *verbal* leave (a `leave` attendance row with no
/// approved leave request behind it) stays freely overwritable. The guard's
/// third branch — `NOT EXISTS (approved leave)` — is true, so a present upsert
/// lands. This preserves the pre-existing correction path for verbal leave.
#[sqlx::test]
async fn upsert_guard_allows_present_over_verbal_leave(db: PgPool) {
    let course_id = seed_course(&db, "Verbal Leave Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = seed_member(&db, "att-guard-verbal@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member, course_id, "active", Utc::now()).await;
    // Verbal leave: an attendance `leave` row, but no approved leave_request.
    seed_attendance(&db, session_id, enrolment_id, "leave", member).await;

    let mut tx = db.begin().await.expect("begin");
    let affected = repository::upsert_attendance_tx(
        &mut tx,
        session_id,
        enrolment_id,
        AttendanceStatus::Present,
        member,
    )
    .await
    .expect("upsert");
    tx.commit().await.expect("commit");

    assert_eq!(affected, 1, "an allowed overwrite must report one row");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&db)
    .await
    .expect("fetch status");
    assert_eq!(status, "present", "verbal leave (no approved request) must stay overwritable");
}

fn present(enrolment_id: Uuid) -> AttendanceRecordEntry {
    AttendanceRecordEntry {
        enrolment_id,
        status: "present".into(),
    }
}

async fn attendance_status(db: &PgPool, session_id: Uuid, enrolment_id: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_optional(db)
    .await
    .expect("fetch status")
}

/// 真併發見證 ADR-0008 決策 3:核准在批次讀完 approved-set 之後、寫到該成員
/// 之前 commit,寫入點守衛擋下 → 整批 422、零寫入。
///
/// 1. T_block 先 INSERT (session, A) 不 commit,讓批次的 A upsert 卡在
///    唯一鍵上——此時批次已讀完 approved-set(∅,B 的假單還是 pending)。
/// 2. 真的 `decide_leave_request` 核准 B 並 commit(B 投影成 `leave`)。
/// 3. T_block rollback,批次繼續:A 寫入、B 的 present 撞上守衛 0 列 → 422。
///
/// `spawn_blocking` + `Handle::block_on` 讓批次跑在真的 OS thread 上(同
/// `service_orders.rs` 的併發測試,`#[sqlx::test]` 是單執行緒 runtime)。
#[sqlx::test]
async fn approval_committed_mid_batch_rolls_back_whole_batch(db: PgPool) {
    let course_id = seed_course(&db, "Mid Batch Approval Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let admin = seed_member(&db, "att-race-admin@example.com", "Password!234").await;
    let member_a = seed_member(&db, "att-race-a@example.com", "Password!234").await;
    let member_b = seed_member(&db, "att-race-b@example.com", "Password!234").await;
    let enrolment_a = seed_enrolment(&db, member_a, course_id, "active", Utc::now()).await;
    let enrolment_b = seed_enrolment(&db, member_b, course_id, "active", Utc::now()).await;
    let leave_b = seed_leave_request(&db, enrolment_b, session_id, "pending").await;

    let mut t_block = db.begin().await.expect("begin t_block");
    sqlx::query(
        "INSERT INTO attendance_records \
         (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         VALUES ($1, $2, $3, 'absent'::attendance_status, $4, NOW(), NOW())",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(enrolment_a)
    .bind(admin)
    .execute(&mut *t_block)
    .await
    .expect("t_block insert");

    let db_batch = Arc::new(db.clone());
    let handle = tokio::runtime::Handle::current();
    let batch = tokio::task::spawn_blocking(move || {
        handle.block_on(attendance_service::bulk_upsert_attendance(
            &db_batch,
            studio_now_utc(Utc::now()),
            &admin_auth(admin),
            session_id,
            vec![present(enrolment_a), present(enrolment_b)],
        ))
    });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !batch.is_finished(),
        "batch must be blocked on A's uncommitted row, after its approved-set read"
    );

    leave_service::decide_leave_request(&db, &admin_auth(admin), leave_b, "approved")
        .await
        .expect("approve B while the batch is blocked");

    t_block.rollback().await.expect("rollback t_block");

    let result = batch.await.expect("join batch");
    assert!(
        matches!(
            result,
            Err(AppError::Validation(ref m))
                if m == "cannot overwrite an approved leave with present/absent"
        ),
        "got: {result:?}"
    );
    assert_eq!(
        attendance_status(&db, session_id, enrolment_a).await,
        None,
        "whole batch rolls back: A must have zero rows"
    );
    assert_eq!(
        attendance_status(&db, session_id, enrolment_b)
            .await
            .as_deref(),
        Some("leave"),
        "B keeps the approval's leave projection"
    );
}

/// 守衛第三分支(口頭 leave → present):沒有核准單撐腰的 `leave` 列,整批
/// 點名照樣可以覆寫成 present。
#[sqlx::test]
async fn bulk_present_over_verbal_leave_overwrites(db: PgPool) {
    let course_id = seed_course(&db, "Bulk Verbal Leave Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let admin = seed_member(&db, "att-bulk-verbal-admin@example.com", "Password!234").await;
    let member = seed_member(&db, "att-bulk-verbal@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member, course_id, "active", Utc::now()).await;
    seed_attendance(&db, session_id, enrolment_id, "leave", admin).await;

    attendance_service::bulk_upsert_attendance(
        &db,
        studio_now_utc(Utc::now()),
        &admin_auth(admin),
        session_id,
        vec![present(enrolment_id)],
    )
    .await
    .expect("verbal leave must be overwritable by a bulk present");

    assert_eq!(
        attendance_status(&db, session_id, enrolment_id)
            .await
            .as_deref(),
        Some("present")
    );
}
