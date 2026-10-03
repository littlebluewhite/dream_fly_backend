//! 出勤寫入(Attendance Records):`attendance_records` 表的唯一 runtime 寫入
//! 者。module 公開、函式收窄(比照 `auth::session`):
//!
//! - [`mark_tx`](`pub(super)`,僅 `attendance::service::bulk_upsert_attendance`):
//!   批次點名。空批次跳過查詢 → valid-set 讀 → approved-set 讀 →
//!   `marking::plan` → 逐列 upsert,全部在呼叫端的寫入 tx 內。
//! - [`project_approved_leave_tx`](`pub(crate)`,僅
//!   `leave::service::decide_tx`):核准請假投影成 `leave` 列。
//!
//! 核准恆勝的雙層防護(ADR-0008 乙案)都住在這裡:第一層是 `marking::plan`
//! 的整批 pre-check(approved-set 在寫入 tx 內讀);第二層是私有
//! [`upsert_tx`] 的 `ON CONFLICT … WHERE` 寫入點守衛,關閉 pre-check 之後才
//! commit 的核准留下的 TOCTOU 窗——它擋下時回報 0 列,[`mark_tx`] 轉成與
//! pre-check 相同的整批 422。
//!
//! 兩個讀取([`find_active_enrolment_ids_in`]、
//! [`find_approved_leave_enrolment_ids_tx`])是 [`mark_tx`] 的私有步驟,都
//! 在 tx 內跑:READ COMMITTED 下每條語句本來就各取快照,valid-set 從 pool
//! 移進 tx 不改錯誤序與狀態碼。seed 的 `insert_attendance_bulk`、fixtures 的
//! `seed_attendance` 是記錄在案的 bypass(ADR-0010)。名冊等讀取照「跨模組
//! 讀表」慣例留在 `attendance::repository`。

use std::collections::HashSet;

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;

use super::marking;
use super::model::AttendanceStatus;

/// 批次點名寫入:對一批已 parse 的 `(enrolment_id, status)` 做成員資格與
/// 核准恆勝檢查,通過才逐列 upsert。任何 `Err` 都在 commit 前返回,呼叫端
/// drop tx 即整批零寫入。
///
/// - 空批次:跳過兩個 DB 讀取(`plan` 對兩個空集合照樣通過)。
/// - `plan` 的 `Err`(成員資格或已核准請假 422)在任何 upsert 之前短路。
/// - upsert 回報 0 列只有一個來源:寫入點守衛擋下一筆在 approved-set 讀取
///   之後才 commit 的核准——轉成與 pre-check 相同的 422
///   ([`marking::APPROVED_LEAVE_OVERWRITE`]),競態窗內外對外行為一致。
pub(super) async fn mark_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    course_id: Uuid,
    parsed: Vec<(Uuid, AttendanceStatus)>,
    marked_by: Uuid,
) -> Result<(), AppError> {
    let (valid, approved): (HashSet<Uuid>, HashSet<Uuid>) = if parsed.is_empty() {
        (HashSet::new(), HashSet::new())
    } else {
        let requested: HashSet<Uuid> = parsed.iter().map(|(id, _)| *id).collect();
        let ids: Vec<Uuid> = requested.into_iter().collect();
        let valid = find_active_enrolment_ids_in(tx, course_id, &ids)
            .await?
            .into_iter()
            .collect();
        let approved = find_approved_leave_enrolment_ids_tx(tx, session_id, &ids)
            .await?
            .into_iter()
            .collect();
        (valid, approved)
    };
    let plan = marking::plan(parsed, &valid, &approved)?;

    for (enrolment_id, status) in &plan.entries {
        let affected = upsert_tx(tx, session_id, *enrolment_id, *status, marked_by).await?;
        if affected == 0 {
            return Err(AppError::Validation(
                marking::APPROVED_LEAVE_OVERWRITE.into(),
            ));
        }
    }
    Ok(())
}

/// 核准請假的出勤投影:在 decide 的同一 tx 內把 `(session_id,
/// enrolment_id)` 寫成 `leave`(`marked_by` = 核准者)。寫 `leave` 恆過守衛
/// 第一分支(核准恆勝,ADR-0008 決策 2),故不回傳列數。
pub(crate) async fn project_approved_leave_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    enrolment_id: Uuid,
    decided_by: Uuid,
) -> Result<(), sqlx::Error> {
    upsert_tx(
        tx,
        session_id,
        enrolment_id,
        AttendanceStatus::Leave,
        decided_by,
    )
    .await?;
    Ok(())
}

/// Of the given `enrolment_ids`, return the subset that both belong to
/// `course_id` and are `active`. [`mark_tx`] hands this to `marking::plan`,
/// which compares it against the full requested set — anything missing is
/// either foreign to the course or not active, and rejects the whole batch
/// before any write.
async fn find_active_enrolment_ids_in(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    enrolment_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM active_enrolments \
         WHERE course_id = $1 AND id = ANY($2::uuid[])",
    )
    .bind(course_id)
    .bind(enrolment_ids)
    .fetch_all(&mut **tx)
    .await
}

/// Of the given `enrolment_ids`, the subset holding an `approved` leave
/// request for `session_id` — `marking::plan`'s third input (the whole-batch
/// half of the 核准恆勝 guard, ADR-0008). Read inside the write tx so `plan`'s
/// verdict and the upserts share one transaction. `$2` is the batch's
/// enrolment ids — the guard and its 422 only concern in-batch members — and
/// the query walks the `uniq_leave_requests_active` partial index
/// (enrolment_id-leading, `approved` ∈ its predicate). Direct read of
/// leave's table per 「跨模組讀表」.
async fn find_approved_leave_enrolment_ids_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    enrolment_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT enrolment_id FROM leave_requests \
         WHERE session_id = $1 AND enrolment_id = ANY($2::uuid[]) \
           AND status = 'approved'::leave_status",
    )
    .bind(session_id)
    .bind(enrolment_ids)
    .fetch_all(&mut **tx)
    .await
}

/// Upsert a single attendance mark within an already-open transaction.
/// `ON CONFLICT DO UPDATE` never touches `created_at`, so the original
/// insert time survives repeated re-marking.
///
/// The `ON CONFLICT ... WHERE` clause is the self-defending write-point half
/// of the 核准恆勝 guard (ADR-0008) — it closes the TOCTOU window that
/// `marking::plan`'s pre-check alone can't (an approval committing between the
/// batch's approved-set read and this upsert). The update is *skipped* when it
/// would overwrite a `leave` row that is backed by an `approved` leave request
/// with a non-`leave` status. Three OR branches, any one allowing the write:
///  - `EXCLUDED.status = 'leave'` — writing `leave` is always allowed
///    ([`project_approved_leave_tx`] always wins; idempotent re-marks of
///    `leave` pass);
///  - `attendance_records.status <> 'leave'` — the existing row isn't a leave,
///    so normal present/absent overwrites are unaffected;
///  - `NOT EXISTS (approved leave)` — the existing `leave` row is verbal (no
///    approved request behind it), so it stays freely overwritable.
///
/// The blocked case affects zero rows *without erroring* — and zero is the
/// block's *only* producer (a conflict-free INSERT and an allowed DO UPDATE
/// both report 1 row), so the returned `rows_affected` is the guard's signal
/// [`mark_tx`] converts into the batch-wide 422. Recovery from the residual
/// snapshot-lag window (ADR-0008 known gap 2) is a manual re-mark to `leave`.
/// The `EXISTS` sub-select walks the same `uniq_leave_requests_active`
/// partial index.
async fn upsert_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    enrolment_id: Uuid,
    status: AttendanceStatus,
    marked_by: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO attendance_records \
         (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         VALUES ($1, $2, $3, $4, $5, NOW(), NOW()) \
         ON CONFLICT (session_id, enrolment_id) DO UPDATE \
         SET status = EXCLUDED.status, marked_by = EXCLUDED.marked_by, marked_at = EXCLUDED.marked_at \
         WHERE EXCLUDED.status = 'leave'::attendance_status \
            OR attendance_records.status <> 'leave'::attendance_status \
            OR NOT EXISTS ( \
                SELECT 1 FROM leave_requests lr \
                WHERE lr.enrolment_id = attendance_records.enrolment_id \
                  AND lr.session_id = attendance_records.session_id \
                  AND lr.status = 'approved'::leave_status \
            )",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(enrolment_id)
    .bind(status)
    .bind(marked_by)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}
