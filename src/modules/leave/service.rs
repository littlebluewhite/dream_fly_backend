use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::modules::attendance::records as attendance_records;
use crate::modules::coaches::service as coaches_service;
use crate::modules::courses::seats;
use crate::modules::notifications::service as notify;
use crate::utils::studio_clock::{self, StudioNow};

use super::dto::{
    AdminLeaveRequestResponse, CreateLeaveRequestRequest, LeaveRequestListResponse,
    LeaveRequestQuery, LeaveRequestResponse, MakeupRequest,
};
use super::model::{LeaveRequestView, LeaveStatus};
use super::repository;
use super::rules;

/// `請假申請不存在` — shared by `cancel_leave_request`, `decide_leave_request`,
/// and `book_makeup`'s initial leave-request lookup.
const LEAVE_NOT_FOUND: &str = "請假申請不存在";

/// `場次不存在` — shared by `create_leave_request`'s session-context lookup
/// and `book_makeup`'s target-session lock.
const SESSION_NOT_FOUND: &str = "場次不存在";

/// `POST /leave-requests`. Resolves the caller's active enrolment from
/// `session_id`'s course (404 `未報名此課程` if none), rejects sessions that
/// have already started (422), and relies on the partial unique index
/// `uniq_leave_requests_active` to reject a duplicate live request (409) —
/// no pre-check SELECT, since the mapped message is identical either way.
pub async fn create_leave_request(
    db: &PgPool,
    at: StudioNow,
    auth: &AuthUser,
    req: CreateLeaveRequestRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let StudioNow { tz, now } = at;
    let session = repository::find_session_context(db, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound(SESSION_NOT_FOUND.into()))?;

    let enrolment_id = repository::find_active_enrolment(db, auth.user_id, session.course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("未報名此課程".into()))?;

    studio_clock::require_not_started(
        tz,
        now,
        session.session_date,
        session.start_time,
        "session time",
        AppError::Validation("場次已開始，無法請假".into()),
    )?;

    let view = repository::insert(db, enrolment_id, req.session_id, req.reason.as_deref())
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "此場次已有請假紀錄"))?;
    Ok(view.into())
}

/// `GET /leave-requests/me` — plain array, newest first (mirrors
/// `enrolments`/`waitlist`'s `/me` convention: no pagination).
pub async fn list_my_leave_requests(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<LeaveRequestResponse>, AppError> {
    let rows = repository::find_my_leave_requests(db, user_id).await?;
    Ok(rows.into_iter().map(LeaveRequestResponse::from).collect())
}

/// `DELETE /leave-requests/{id}` — owner only (no admin bypass: the brief
/// scopes this endpoint to `member, owner`, unlike the coach/admin-scoped
/// list and decide endpoints below), and only while still `pending`.
pub async fn cancel_leave_request(db: &PgPool, auth: &AuthUser, id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin().await?;

    let owner = repository::find_owner_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound(LEAVE_NOT_FOUND.into()))?;

    auth.owner_only(owner.user_id, "僅本人可取消請假申請")?;

    repository::cancel_if_pending_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::Conflict("僅待審核假單可取消".into()))?;

    tx.commit().await?;
    Ok(())
}

/// Passthrough to `repository::cancel_pending_for_enrolments_tx` — the
/// ADR-0005 seam `enrolments::service` calls when an enrolment is cancelled
/// (self-cancel or order compensation), so the enrolment's still-pending
/// leave requests are cancelled in the same tx (B5). Sends no notification.
pub async fn cancel_pending_for_enrolments_tx(
    tx: &mut Transaction<'_, Postgres>,
    enrolment_ids: &[Uuid],
) -> Result<u64, AppError> {
    repository::cancel_pending_for_enrolments_tx(tx, enrolment_ids)
        .await
        .map_err(AppError::Database)
}

/// `GET /leave-requests?status=&course_id=` — coach (own courses only) or
/// admin (all courses). A coach with no `coaches` row degrades to an empty
/// page rather than erroring, mirroring `sessions::today_sessions`'s
/// convention for that same data anomaly (this is a scoped list, not a
/// single-resource ownership check, so 403 isn't the right shape here).
pub async fn list_leave_requests(
    db: &PgPool,
    auth: &AuthUser,
    query: LeaveRequestQuery,
    pagination: &PaginationParams,
) -> Result<LeaveRequestListResponse, AppError> {
    let status_filter = match &query.status {
        Some(s) => {
            let parsed: LeaveStatus = s
                .parse()
                .map_err(|_| AppError::Validation(format!("status 參數不正確：{s}")))?;
            Some(parsed)
        }
        None => None,
    };

    let limit = pagination.limit();

    let coach_scope = if auth.is_admin() {
        None
    } else {
        let Some(scope) = coaches_service::resolve_scope(db, auth).await? else {
            return Ok(LeaveRequestListResponse {
                leave_requests: Vec::new(),
                meta: pagination.meta(0),
            });
        };
        Some(scope)
    };

    let total =
        repository::count_admin_list(db, status_filter, query.course_id, coach_scope.as_ref()).await?;
    let rows = repository::find_admin_list(
        db,
        status_filter,
        query.course_id,
        coach_scope.as_ref(),
        limit,
        pagination.offset(),
    )
    .await?;

    Ok(LeaveRequestListResponse {
        leave_requests: rows.into_iter().map(AdminLeaveRequestResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

/// 核准/駁回一張 `pending` 假單,核准時在同一 tx 投影出勤 `leave`(核准恆勝,
/// ADR-0008)。`decide_leave_request` 與測試 fixture(`seed_leave_request`)共用
/// 這個 seam,所以 fixture 造出的 Approved 假單與 production 狀態一致。已非
/// `pending` 時回 `None`(呼叫端決定 409 或 panic)。
pub async fn decide_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    new_status: LeaveStatus,
    decided_by: Uuid,
) -> Result<Option<LeaveRequestView>, sqlx::Error> {
    let Some(updated) = repository::decide_tx(tx, id, new_status, decided_by).await? else {
        return Ok(None);
    };
    if new_status == LeaveStatus::Approved {
        let enrolment_id = repository::find_enrolment_id_tx(tx, id).await?;
        attendance_records::project_approved_leave_tx(
            tx,
            updated.session_id,
            enrolment_id,
            decided_by,
        )
        .await?;
    }
    Ok(Some(updated))
}

/// `PATCH /leave-requests/{id}` — that course's coach or admin decides a
/// still-`pending` request. Approving upserts `attendance_records.status =
/// 'leave'` for the original session in the *same transaction* as the
/// status update (task brief: "approve 同一 tx"); rejecting touches no
/// attendance row. The notification is written *after* commit, synchronously,
/// via the existing `notifications::service` seam — see this task's report
/// for the tradeoff (matches every other caller of that seam, e.g.
/// `bookings::service::create_booking`, which also notifies post-commit).
pub async fn decide_leave_request(
    db: &PgPool,
    auth: &AuthUser,
    id: Uuid,
    new_status_str: &str,
) -> Result<LeaveRequestResponse, AppError> {
    let new_status = rules::parse_decision(new_status_str)?;

    let ctx = repository::find_decision_context(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound(LEAVE_NOT_FOUND.into()))?;

    coaches_service::require_course_coach(db, auth, ctx.coach_id, "非本課教練").await?;

    rules::check_decidable(&ctx, new_status)?;

    let mut tx = db.begin().await?;

    let updated = decide_tx(&mut tx, id, new_status, auth.user_id)
        .await?
        .ok_or_else(|| AppError::Conflict(rules::DECIDE_NOT_PENDING.into()))?;

    tx.commit().await?;

    notify::leave_request_decided(
        ctx.user_id,
        new_status == LeaveStatus::Approved,
        &ctx.course_name,
        ctx.session_date,
    )
    .deliver(db)
    .await;

    // A just-decided request was `pending`, so it can carry no booked makeup
    // yet (makeup requires an already-approved request) — `updated`'s LEFT
    // JOINed makeup columns are `NULL` accordingly, matching contract §3.20's
    // "此時 makeup_session_id 等欄位必為 null".
    Ok(updated.into())
}

/// `POST /leave-requests/{id}/makeup` — owner only. Two row locks make the
/// check-then-write sequence race-free (controller ruling 2026-07-06):
/// the leave-request row lock (`find_for_makeup_tx`) serializes two
/// concurrent calls for the *same* request (only the first can see
/// `makeup_session_id IS NULL`), and the target-session row lock
/// (`seats::lock_session_tx`, taken before the seat count) serializes
/// *different* leave requests racing for the same session's last free seat.
///
/// Seat check — physical seat model (controller ruling 2026-07-06): of the
/// course's `max_students` seats at the target session, every active
/// enrolment occupies one, every approved leave *for that session* frees
/// one, and every makeup already booked into it takes one back:
/// `max_students - active_count + approved_leave_count - makeup_count > 0`.
/// Both counts consider only still-active enrolments (see
/// `seats::require_room_tx`, which owns the formula and its 409).
///
/// Error precedence: leave request 404 → owner 403 → source 409 → target
/// session 404 → target rules 422/400 → seats 409. The target session is
/// read once, by its row lock (`SessionLock::session`).
pub async fn book_makeup(
    db: &PgPool,
    at: StudioNow,
    auth: &AuthUser,
    id: Uuid,
    req: MakeupRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let mut tx = db.begin().await?;

    let leave = repository::find_for_makeup_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound(LEAVE_NOT_FOUND.into()))?;

    auth.owner_only(leave.user_id, "僅本人可預約補課")?;
    rules::check_makeup_source(&leave)?;

    // Serialize concurrent makeups into the same target session across
    // *different* leave requests before counting seats — the leave-request
    // row lock above only defends re-booking of the same request.
    let lock = seats::lock_session_tx(&mut tx, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound(SESSION_NOT_FOUND.into()))?;

    rules::check_makeup_target(&leave, lock.session(), at)?;

    seats::require_room_tx(&mut tx, &lock).await?;

    let updated = repository::set_makeup_session_tx(&mut tx, id, req.session_id)
        .await?
        .ok_or_else(|| AppError::Conflict(rules::MAKEUP_ALREADY_BOOKED.into()))?;

    tx.commit().await?;

    // `updated`'s makeup columns now resolve against the just-written
    // `makeup_session_id` (the write and this read share the data-modifying
    // CTE's one statement), so the booked target's date/time need no
    // separate assembly from `lock.session()` here.
    Ok(updated.into())
}
