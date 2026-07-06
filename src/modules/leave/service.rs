use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::modules::attendance::model::AttendanceStatus;
use crate::modules::attendance::repository as attendance_repository;
use crate::modules::coaches::repository as coaches_repository;
use crate::modules::notifications::service as notify;

use super::dto::{
    AdminLeaveRequestResponse, CreateLeaveRequestRequest, LeaveRequestListResponse,
    LeaveRequestQuery, LeaveRequestResponse, MakeupRequest,
};
use super::model::LeaveStatus;
use super::repository;

/// Resolve the studio timezone. Mirrors `sessions::service::studio_tz` /
/// `bookings::service::studio_tz` (each module keeps its own tiny copy —
/// established convention in this codebase rather than a shared helper).
/// Startup validation (`AppConfig::load`) already rejects invalid timezone
/// names, so the UTC fallback only fires if a future refactor bypasses that.
fn studio_tz(server: &ServerConfig) -> Tz {
    server.studio_timezone.parse::<Tz>().unwrap_or(chrono_tz::UTC)
}

/// Whether a session's (date, start_time) — interpreted as studio-local
/// wall-clock time, per contract §3.18 裁決 2/裁決 4 — is at or before `now`.
/// Mirrors `bookings::service::create_booking`'s slot-start check: convert
/// the naive local instant to UTC via the studio tz (erroring on an
/// ambiguous DST-transition instant, which won't occur in the test harness's
/// UTC-pinned config) rather than converting `now` to naive local, so the
/// comparison is unambiguous regardless of `tz`. `now` is a parameter
/// (rather than calling `Utc::now()` internally) so this is unit-testable
/// with fixed instants — mirrors `sessions::service::studio_date_at`.
fn session_has_started(
    tz: Tz,
    now: DateTime<Utc>,
    date: NaiveDate,
    time: NaiveTime,
) -> Result<bool, AppError> {
    let local = NaiveDateTime::new(date, time);
    match tz.from_local_datetime(&local).single() {
        Some(dt) => Ok(dt.with_timezone(&Utc) <= now),
        None => Err(AppError::BadRequest(
            "session time falls on an ambiguous local time".into(),
        )),
    }
}

/// Shared coach-ownership gate for the list/decide endpoints: an admin
/// always passes; a coach passes only if the course's `coach_id` matches
/// their own `coaches.id`. Mirrors
/// `attendance::service::authorize_session_coach` (copied rather than
/// shared — each module keeps its own small copy, same convention as
/// `studio_tz` above).
async fn authorize_course_coach(
    db: &PgPool,
    auth: &AuthUser,
    course_coach_id: Option<Uuid>,
) -> Result<(), AppError> {
    if auth.is_admin() {
        return Ok(());
    }

    let is_owner = match (
        coaches_repository::find_by_user_id(db, auth.user_id).await?,
        course_coach_id,
    ) {
        (Some(coach), Some(course_coach_id)) => coach.id == course_coach_id,
        _ => false,
    };

    if is_owner {
        Ok(())
    } else {
        Err(AppError::Forbidden("非本課教練".into()))
    }
}

/// `POST /leave-requests`. Resolves the caller's active enrolment from
/// `session_id`'s course (404 `未報名此課程` if none), rejects sessions that
/// have already started (422), and relies on the partial unique index
/// `uniq_leave_requests_active` to reject a duplicate live request (409) —
/// no pre-check SELECT, since the mapped message is identical either way.
pub async fn create_leave_request(
    db: &PgPool,
    server: &ServerConfig,
    auth: &AuthUser,
    req: CreateLeaveRequestRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let session = repository::find_session_context(db, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("場次不存在".into()))?;

    let enrolment_id = repository::find_active_enrolment(db, auth.user_id, session.course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("未報名此課程".into()))?;

    if session_has_started(
        studio_tz(server),
        Utc::now(),
        session.session_date,
        session.start_time,
    )? {
        return Err(AppError::Validation("場次已開始，無法請假".into()));
    }

    match repository::insert(db, enrolment_id, req.session_id, req.reason.as_deref()).await {
        Ok(lr) => Ok(LeaveRequestResponse {
            id: lr.id,
            course_id: session.course_id,
            course_name: session.course_name,
            session_id: lr.session_id,
            session_date: session.session_date,
            start_time: session.start_time,
            reason: lr.reason,
            status: lr.status.as_str().to_string(),
            makeup_session_id: None,
            makeup_session_date: None,
            makeup_start_time: None,
            decided_at: None,
            created_at: lr.created_at,
        }),
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            Err(AppError::Conflict("此場次已有請假紀錄".into()))
        }
        Err(e) => Err(AppError::Database(e)),
    }
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

    if owner.user_id != auth.user_id {
        return Err(AppError::Forbidden("僅本人可取消請假申請".into()));
    }

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
        match coaches_repository::find_by_user_id(db, auth.user_id).await? {
            Some(coach) => Some(coach.id),
            None => {
                return Ok(LeaveRequestListResponse {
                    leave_requests: Vec::new(),
                    total: 0,
                    page,
                    per_page: limit,
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
        total,
        page,
        per_page: limit,
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

    authorize_course_coach(db, auth, ctx.coach_id).await?;

    if ctx.status != LeaveStatus::Pending {
        return Err(AppError::Conflict("僅待審核假單可審核".into()));
    }

    let mut tx = db.begin().await?;

    let updated = repository::decide_tx(&mut tx, id, new_status, auth.user_id)
        .await?
        .ok_or_else(|| AppError::Conflict("僅待審核假單可審核".into()))?;

    if new_status == LeaveStatus::Approved {
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
        db,
        ctx.user_id,
        new_status == LeaveStatus::Approved,
        &ctx.course_name,
        ctx.session_date,
    )
    .await;

    Ok(LeaveRequestResponse {
        id: updated.id,
        course_id: ctx.course_id,
        course_name: ctx.course_name,
        session_id: ctx.session_id,
        session_date: ctx.session_date,
        start_time: ctx.start_time,
        reason: updated.reason,
        status: updated.status.as_str().to_string(),
        makeup_session_id: updated.makeup_session_id,
        makeup_session_date: None,
        makeup_start_time: None,
        decided_at: updated.decided_at,
        created_at: updated.created_at,
    })
}

/// `POST /leave-requests/{id}/makeup` — owner only. Locks the leave request
/// row for the whole check-then-write sequence so two concurrent calls for
/// the *same* request serialize (only the first can see `makeup_session_id
/// IS NULL`); see the task report for why this does *not* also defend
/// against two *different* leave requests racing for the same target
/// session's last capacity slot (not required by the task brief/tests).
///
/// Capacity formula (task brief, verbatim): a makeup booking is allowed iff
/// `max_students - active_count - approved_leave_count + makeup_count > 0`,
/// where `active_count`/`approved_leave_count`/`makeup_count` are all scoped
/// to the *target* session/course. Flagged in the task report: a naive
/// physical-occupancy model would add `approved_leave_count` (a leave-taker
/// frees a seat) and subtract `makeup_count` (a makeup booking fills one) —
/// the opposite of the brief's literal signs. Implemented exactly as
/// specified; worth a product-owner double-check.
pub async fn book_makeup(
    db: &PgPool,
    server: &ServerConfig,
    auth: &AuthUser,
    id: Uuid,
    req: MakeupRequest,
) -> Result<LeaveRequestResponse, AppError> {
    let tz = studio_tz(server);
    let mut tx = db.begin().await?;

    let leave = repository::find_for_makeup_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("請假申請不存在".into()))?;

    if leave.user_id != auth.user_id {
        return Err(AppError::Forbidden("僅本人可預約補課".into()));
    }
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

    if session_has_started(tz, Utc::now(), target.session_date, target.start_time)? {
        return Err(AppError::Validation("補課場次已開始".into()));
    }

    let capacity = repository::find_makeup_capacity_tx(&mut tx, target.course_id, req.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("課程不存在".into()))?;

    let remaining = capacity.max_students as i64 - capacity.active_count
        - capacity.approved_leave_count
        + capacity.makeup_count;
    if remaining <= 0 {
        return Err(AppError::Conflict("該場次名額已滿".into()));
    }

    let updated = repository::set_makeup_session_tx(&mut tx, id, req.session_id)
        .await?
        .ok_or_else(|| AppError::Conflict("此假單已預約過補課".into()))?;

    tx.commit().await?;

    Ok(LeaveRequestResponse {
        id: updated.id,
        course_id: leave.course_id,
        course_name: leave.course_name,
        session_id: leave.session_id,
        session_date: leave.session_date,
        start_time: leave.start_time,
        reason: updated.reason,
        status: updated.status.as_str().to_string(),
        makeup_session_id: updated.makeup_session_id,
        makeup_session_date: Some(target.session_date),
        makeup_start_time: Some(target.start_time),
        decided_at: updated.decided_at,
        created_at: updated.created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn session_has_started_false_when_now_is_before_start() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 8, 0, 0).unwrap();
        assert!(!session_has_started(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)).unwrap());
    }

    #[test]
    fn session_has_started_true_when_now_is_at_or_after_start() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        assert!(session_has_started(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)).unwrap());

        let later = Utc.with_ymd_and_hms(2026, 7, 5, 9, 30, 0).unwrap();
        assert!(session_has_started(chrono_tz::UTC, later, d(2026, 7, 5), t(9, 0)).unwrap());
    }

    #[test]
    fn session_has_started_uses_studio_local_wall_clock_not_utc_date() {
        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th (UTC+8). A session
        // dated the 6th at 08:00 Taipei-local has NOT started yet at that
        // instant, even though the UTC calendar date is still the 5th.
        let taipei = "Asia/Taipei".parse::<Tz>().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 23, 30, 0).unwrap();
        assert!(!session_has_started(taipei, now, d(2026, 7, 6), t(8, 0)).unwrap());

        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th — a session dated
        // the 6th at 07:00 Taipei-local HAS already started.
        assert!(session_has_started(taipei, now, d(2026, 7, 6), t(7, 0)).unwrap());
    }
}
