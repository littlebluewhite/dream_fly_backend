use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::{PageMeta, PaginationParams};
use crate::modules::attendance::model::AttendanceStatus;
use crate::modules::attendance::repository as attendance_repository;
use crate::modules::coaches::service as coaches_service;
use crate::modules::courses::seats;
use crate::modules::notifications::service as notify;
use crate::utils::studio_clock;

use super::dto::{
    AdminLeaveRequestResponse, CreateLeaveRequestRequest, LeaveRequestListResponse,
    LeaveRequestParts, LeaveRequestQuery, LeaveRequestResponse, MakeupInfo, MakeupRequest,
};
use super::model::LeaveStatus;
use super::repository;

/// `POST /leave-requests`. Resolves the caller's active enrolment from
/// `session_id`'s course (404 `未報名此課程` if none), rejects sessions that
/// have already started (422), and relies on the partial unique index
/// `uniq_leave_requests_active` to reject a duplicate live request (409) —
/// no pre-check SELECT, since the mapped message is identical either way.
pub async fn create_leave_request(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    auth: &AuthUser,
    req: CreateLeaveRequestRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let session = repository::find_session_context(db, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("場次不存在".into()))?;

    let enrolment_id = repository::find_active_enrolment(db, auth.user_id, session.course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("未報名此課程".into()))?;

    studio_clock::require_not_started(
        studio_clock::studio_tz(server),
        now,
        session.session_date,
        session.start_time,
        "session time",
        AppError::Validation("場次已開始，無法請假".into()),
    )?;

    let lr = repository::insert(db, enrolment_id, req.session_id, req.reason.as_deref())
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "此場次已有請假紀錄"))?;
    Ok(LeaveRequestParts {
        id: lr.id,
        course_id: session.course_id,
        course_name: session.course_name,
        session_id: lr.session_id,
        session_date: session.session_date,
        start_time: session.start_time,
        reason: lr.reason,
        status: lr.status.as_str().to_string(),
        makeup: None,
        decided_at: None,
        created_at: lr.created_at,
    }
    .into())
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
        .ok_or_else(|| AppError::NotFound("請假申請不存在".into()))?;

    auth.owner_only(owner.user_id, "僅本人可取消請假申請")?;

    repository::cancel_if_pending_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::Conflict("僅待審核假單可取消".into()))?;

    tx.commit().await?;
    Ok(())
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
            Some(parsed.as_str())
        }
        None => None,
    };

    let limit = pagination.limit();
    let page = pagination.page.max(1);

    let coach_scope: Option<Uuid> = if auth.is_admin() {
        None
    } else {
        match coaches_service::resolve(db, auth).await? {
            Some(coach) => Some(coach.id),
            None => {
                return Ok(LeaveRequestListResponse {
                    leave_requests: Vec::new(),
                    meta: PageMeta {
                        total: 0,
                        page,
                        per_page: limit,
                    },
                });
            }
        }
    };

    let total =
        repository::count_admin_list(db, status_filter, query.course_id, coach_scope).await?;
    let rows = repository::find_admin_list(
        db,
        status_filter,
        query.course_id,
        coach_scope,
        limit,
        pagination.offset(),
    )
    .await?;

    Ok(LeaveRequestListResponse {
        leave_requests: rows.into_iter().map(AdminLeaveRequestResponse::from).collect(),
        meta: PageMeta {
            total,
            page,
            per_page: limit,
        },
    })
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
    let new_status = match new_status_str {
        "approved" => LeaveStatus::Approved,
        "rejected" => LeaveStatus::Rejected,
        _ => {
            return Err(AppError::Validation(
                "status 僅接受 approved 或 rejected".into(),
            ));
        }
    };

    let ctx = repository::find_decision_context(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("請假申請不存在".into()))?;

    coaches_service::require_course_coach(db, auth, ctx.coach_id, "非本課教練").await?;

    if ctx.status != LeaveStatus::Pending {
        return Err(AppError::Conflict("僅待審核假單可審核".into()));
    }

    let mut tx = db.begin().await?;

    let updated = repository::decide_tx(&mut tx, id, new_status, auth.user_id)
        .await?
        .ok_or_else(|| AppError::Conflict("僅待審核假單可審核".into()))?;

    if new_status == LeaveStatus::Approved {
        // Writing `leave` always passes the upsert guard's first branch
        // (`EXCLUDED.status = 'leave'` — 核准恆勝, ADR-0008), so the
        // rows_affected signal the bulk-marking path checks is always 1 here.
        attendance_repository::upsert_attendance_tx(
            &mut tx,
            ctx.session_id,
            ctx.enrolment_id,
            AttendanceStatus::Leave,
            auth.user_id,
        )
        .await?;
    }

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
    // yet (makeup requires an already-approved request) — `None` here, matching
    // contract §3.20's "此時 makeup_session_id 等欄位必為 null". The
    // `Option<MakeupInfo>` constructor makes that null-triple explicit rather
    // than passing a lone `updated.makeup_session_id` with null date/time.
    Ok(LeaveRequestParts {
        id: updated.id,
        course_id: ctx.course_id,
        course_name: ctx.course_name,
        session_id: ctx.session_id,
        session_date: ctx.session_date,
        start_time: ctx.start_time,
        reason: updated.reason,
        status: updated.status.as_str().to_string(),
        makeup: None,
        decided_at: updated.decided_at,
        created_at: updated.created_at,
    }
    .into())
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
/// `seats::session_seats_tx`).
pub async fn book_makeup(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    auth: &AuthUser,
    id: Uuid,
    req: MakeupRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let tz = studio_clock::studio_tz(server);
    let mut tx = db.begin().await?;

    let leave = repository::find_for_makeup_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("請假申請不存在".into()))?;

    auth.owner_only(leave.user_id, "僅本人可預約補課")?;
    if leave.status != LeaveStatus::Approved {
        return Err(AppError::Conflict("僅已核准的假單可預約補課".into()));
    }
    if leave.makeup_session_id.is_some() {
        return Err(AppError::Conflict("此假單已預約過補課".into()));
    }

    let target = repository::find_session_context_tx(&mut tx, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("場次不存在".into()))?;

    if target.course_id != leave.course_id {
        return Err(AppError::Validation("補課場次須為同一課程".into()));
    }

    studio_clock::require_not_started(
        tz,
        now,
        target.session_date,
        target.start_time,
        "session time",
        AppError::Validation("補課場次已開始".into()),
    )?;

    // Serialize concurrent makeups into the same target session across
    // *different* leave requests before counting seats — the leave-request
    // row lock above only defends re-booking of the same request.
    let lock = seats::lock_session_tx(&mut tx, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("場次不存在".into()))?;

    let session_seats = seats::session_seats_tx(&mut tx, &lock)
        .await?
        .ok_or_else(|| AppError::NotFound("課程不存在".into()))?;

    // Physical seat model: leave for the target frees a seat, an existing
    // makeup into it occupies one (controller ruling 2026-07-06).
    if session_seats.remaining() <= 0 {
        return Err(AppError::Conflict("該場次名額已滿".into()));
    }

    let updated = repository::set_makeup_session_tx(&mut tx, id, req.session_id)
        .await?
        .ok_or_else(|| AppError::Conflict("此假單已預約過補課".into()))?;

    tx.commit().await?;

    Ok(LeaveRequestParts {
        id: updated.id,
        course_id: leave.course_id,
        course_name: leave.course_name,
        session_id: leave.session_id,
        session_date: leave.session_date,
        start_time: leave.start_time,
        reason: updated.reason,
        status: updated.status.as_str().to_string(),
        makeup: Some(MakeupInfo {
            session_id: req.session_id,
            session_date: target.session_date,
            start_time: target.start_time,
        }),
        decided_at: updated.decided_at,
        created_at: updated.created_at,
    }
    .into())
}
