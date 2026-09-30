//! Service-level tests for `attendance::records`' 核准恆勝 guard (ADR-0008),
//! driven through `attendance::service::bulk_upsert_attendance`: a genuine
//! concurrent witness that the write-point guard's zero-row block surfaces as
//! the same batch-wide 422 as the pre-check, and the guard's verbal-leave
//! branch; plus the batch's 403 → not-started → status-parse error-precedence
//! pins.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::attendance::dto::AttendanceRecordEntry;
use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::attendance::service as attendance_service;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::leave::service as leave_service;

use common::fixtures::{
    seed_attendance, seed_coach, seed_course, seed_course_session, seed_enrolment, seed_leave_request,
};
use common::{admin_auth, coach_auth, seed_member, studio_now_utc};

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn yesterday() -> chrono::NaiveDate {
    (Utc::now() - Duration::days(1)).date_naive()
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
    let enrolment_a =
        seed_enrolment(&db, member_a, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&db, member_b, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let leave_b = seed_leave_request(&db, enrolment_b, session_id, LeaveStatus::Pending).await;

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
    let t_block_pid = common::backend_pid(&mut t_block).await;

    let db_batch = db.clone();
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

    common::wait_for_lock_waiter(&db, t_block_pid).await;
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
    let enrolment_id =
        seed_enrolment(&db, member, course_id, EnrolmentStatus::Active, Utc::now()).await;
    seed_attendance(&db, session_id, enrolment_id, AttendanceStatus::Leave, admin).await;

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

fn tomorrow() -> chrono::NaiveDate {
    (Utc::now() + Duration::days(1)).date_naive()
}

/// 錯誤優先序:不是本課教練(403)先於場次尚未開始(422)。
#[sqlx::test]
async fn bulk_upsert_forbidden_precedes_not_started(db: PgPool) {
    let coach_user = seed_member(&db, "att-pin-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user, "Head Coach").await;
    let course_id = seed_course(&db, "Bulk Forbidden Pin Course", Some(coach_id)).await;
    let session_id = seed_course_session(&db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let outsider = seed_member(&db, "att-pin-outsider@example.com", "Password!234").await;
    let member = seed_member(&db, "att-pin-forbidden-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&db, member, course_id, EnrolmentStatus::Active, Utc::now()).await;

    let err = attendance_service::bulk_upsert_attendance(
        &db,
        studio_now_utc(Utc::now()),
        &coach_auth(outsider),
        session_id,
        vec![present(enrolment_id)],
    )
    .await
    .expect_err("non-owning coach must be rejected");
    assert!(
        matches!(err, AppError::Forbidden(ref m) if m == "not the coach for this course"),
        "got: {err:?}"
    );
}

/// 錯誤優先序:場次尚未開始(422)先於 status 解析錯誤(422,不同文案)。
#[sqlx::test]
async fn bulk_upsert_not_started_precedes_invalid_status(db: PgPool) {
    let course_id = seed_course(&db, "Bulk Not Started Pin Course", None).await;
    let session_id = seed_course_session(&db, course_id, tomorrow(), t(9, 0), t(10, 0)).await;
    let admin = seed_member(&db, "att-pin-admin@example.com", "Password!234").await;
    let member = seed_member(&db, "att-pin-status-member@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&db, member, course_id, EnrolmentStatus::Active, Utc::now()).await;

    let err = attendance_service::bulk_upsert_attendance(
        &db,
        studio_now_utc(Utc::now()),
        &admin_auth(admin),
        session_id,
        vec![AttendanceRecordEntry {
            enrolment_id,
            status: "bogus".into(),
        }],
    )
    .await
    .expect_err("not-started session must be rejected");
    assert!(
        matches!(err, AppError::Validation(ref m) if m == "場次尚未開始，無法點名"),
        "got: {err:?}"
    );
}
