//! 請假規則 (Leave Rules) — the pure decision core of the makeup/decide
//! endpoints, pulled out of the `service` bodies into three pure functions
//! (sibling of `attendance::marking`): [`parse_decision`] turns
//! `PATCH /leave-requests/{id}`'s raw `status` string into a [`LeaveStatus`]
//! (422 on anything but `approved`/`rejected` — deliberately narrower than
//! `LeaveStatus`'s own `FromStr`, which also accepts `pending`/`cancelled`),
//! [`check_decidable`] checks a `PATCH /leave-requests/{id}` decision is
//! still legal (409 if not `pending`, 409 if approving a leave whose
//! enrolment has since been cancelled — rejecting a cancelled enrolment's
//! leave is still allowed, task 3), [`check_makeup_source`] checks a locked
//! leave request is eligible to receive a makeup booking (409 if not
//! `approved`, 409 if it already has one, 409 if its enrolment has since
//! been cancelled), and [`check_makeup_target`] checks the caller-resolved
//! target session against that leave request (422 if it's a different
//! course, 422/400 if it has already started or its start time is
//! DST-ambiguous). Same
//! shape as `orders::pricing`/`orders::fulfilment`: pure function, zero DB,
//! zero async — `service::book_makeup` still owns everything genuinely
//! transactional: the two row locks (`repository::find_for_makeup_tx`,
//! `courses::seats::lock_session_tx`), the target-session DB read, the seat
//! count, and the write.
//!
//! **[`check_makeup_source`] and [`check_makeup_target`] are split in two
//! because a DB read for the target session sits between them, and error
//! ordering is load-bearing.** `service::book_makeup` must reject a
//! not-approved or already-made-up leave request (409) *before* ever
//! looking up the target session (which can itself 404) — a client
//! forgetting to check its own leave request's status shouldn't get a 404
//! for a session id it may not have even sent yet in a hypothetical replay.
//! Cancellation's own guard (`repository::cancel_if_pending_tx`'s `WHERE
//! status = 'pending'`, backed by `find_owner_tx`'s `FOR UPDATE`) is
//! untouched here — it's already a single SQL-enforced check, and wrapping
//! it in a pure function here would just be dead code alongside it.

use crate::error::AppError;
use crate::utils::studio_clock::{self, StudioNow};

use super::model::{LeaveDecisionContext, LeaveRequestForMakeup, LeaveStatus, SessionContext};

/// `僅待審核假單可審核` — shared between [`check_decidable`]'s not-pending
/// check and `repository::decide_tx`'s race fallback (moved here from
/// `service` — task 3 — so the pure decision core owns its own error text).
pub const DECIDE_NOT_PENDING: &str = "僅待審核假單可審核";

/// `此假單已預約過補課` — shared between [`check_makeup_source`]'s
/// already-booked check and `service::book_makeup`'s post-write guard
/// fallback (the `WHERE makeup_session_id IS NULL` race `set_makeup_session_tx`
/// closes should not occur in practice, but the message must still match).
pub const MAKEUP_ALREADY_BOOKED: &str = "此假單已預約過補課";

/// Parse `PATCH /leave-requests/{id}`'s raw `status` string. Only
/// `approved`/`rejected` are accepted (422 on anything else, including
/// `pending`/`cancelled` — valid [`LeaveStatus`] values, just not ones a
/// human decision can produce) — deliberately not `LeaveStatus::from_str`,
/// whose accepted set is the whole enum.
pub fn parse_decision(s: &str) -> Result<LeaveStatus, AppError> {
    match s {
        "approved" => Ok(LeaveStatus::Approved),
        "rejected" => Ok(LeaveStatus::Rejected),
        _ => Err(AppError::Validation(
            "status 僅接受 approved 或 rejected".into(),
        )),
    }
}

/// Check a `PATCH /leave-requests/{id}` decision is still legal for a
/// just-read [`LeaveDecisionContext`]: the request must still be `pending`
/// (409 [`DECIDE_NOT_PENDING`] otherwise), and approving it requires the
/// leave's enrolment to still be active (409 `報名已取消，無法核准請假`
/// otherwise) — rejecting a cancelled enrolment's leave is still allowed,
/// since rejection doesn't grant anything back. Order: not-pending first,
/// then the enrolment check — a raced/already-decided request must surface
/// that error regardless of enrolment state.
///
/// B5 起,報名取消同 tx 已把待審假單轉成 `cancelled`,所以取消之後的審核
/// 走的是 not-pending 409;報名 409 降為併發 backstop,只擋兩種列:取消
/// commit 之後才插入的 pending 列,以及上線前遺留的資料。
pub fn check_decidable(ctx: &LeaveDecisionContext, decision: LeaveStatus) -> Result<(), AppError> {
    if ctx.status != LeaveStatus::Pending {
        return Err(AppError::Conflict(DECIDE_NOT_PENDING.into()));
    }
    if decision == LeaveStatus::Approved && !ctx.enrolment_active {
        return Err(AppError::Conflict("報名已取消，無法核准請假".into()));
    }
    Ok(())
}

/// Check a locked leave request is eligible to receive a makeup booking:
/// must be `approved` (409 otherwise — a `pending`/`rejected`/`cancelled`
/// request can't be made up), must not already carry a `makeup_session_id`
/// (409 — one makeup per leave request), and its enrolment must still be
/// active (409 `報名已取消，無法預約補課` otherwise — task 3). Order matches
/// `service::book_makeup`'s original inline checks: status first, then
/// already-booked, then the enrolment check.
pub fn check_makeup_source(leave: &LeaveRequestForMakeup) -> Result<(), AppError> {
    if leave.status != LeaveStatus::Approved {
        return Err(AppError::Conflict("僅已核准的假單可預約補課".into()));
    }
    if leave.makeup_session_id.is_some() {
        return Err(AppError::Conflict(MAKEUP_ALREADY_BOOKED.into()));
    }
    if !leave.enrolment_active {
        return Err(AppError::Conflict("報名已取消，無法預約補課".into()));
    }
    Ok(())
}

/// Check the caller-resolved target session against a leave request already
/// passed through [`check_makeup_source`]: the target must belong to the
/// same course as the original leave (422 otherwise — no cross-course
/// makeups), must not be the very session the leave was taken for (422
/// `補課場次不可為請假場次` — a makeup into the leave's own session would
/// just cancel the leave, not make it up), and must not have already
/// started (422 `補課場次已開始`, or 400 if its studio-local start time is
/// DST-ambiguous — see `studio_clock::require_not_started`). Order matches
/// `service::book_makeup`'s original inline checks: course, then own-session,
/// then already-started.
pub fn check_makeup_target(
    leave: &LeaveRequestForMakeup,
    target: &SessionContext,
    at: StudioNow,
) -> Result<(), AppError> {
    let StudioNow { tz, now } = at;
    if target.course_id != leave.course_id {
        return Err(AppError::Validation("補課場次須為同一課程".into()));
    }
    if target.id == leave.session_id {
        return Err(AppError::Validation("補課場次不可為請假場次".into()));
    }
    studio_clock::require_not_started(
        tz,
        now,
        target.session_date,
        target.start_time,
        "session time",
        AppError::Validation("補課場次已開始".into()),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
    use uuid::Uuid;

    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn at(now: DateTime<Utc>) -> StudioNow {
        StudioNow {
            tz: chrono_tz::UTC,
            now,
        }
    }

    fn leave(status: LeaveStatus, makeup_session_id: Option<Uuid>) -> LeaveRequestForMakeup {
        LeaveRequestForMakeup {
            id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            course_id: Uuid::now_v7(),
            course_name: "Course".into(),
            status,
            makeup_session_id,
            session_date: d(2026, 7, 5),
            start_time: t(9, 0),
            reason: None,
            enrolment_active: true,
        }
    }

    fn decision_ctx(status: LeaveStatus, enrolment_active: bool) -> LeaveDecisionContext {
        LeaveDecisionContext {
            status,
            enrolment_id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            course_id: Uuid::now_v7(),
            course_name: "Course".into(),
            coach_id: Some(Uuid::now_v7()),
            session_date: d(2026, 7, 5),
            start_time: t(9, 0),
            enrolment_active,
        }
    }

    fn session_context(course_id: Uuid, session_date: NaiveDate, start_time: NaiveTime) -> SessionContext {
        SessionContext {
            id: Uuid::now_v7(),
            course_id,
            course_name: "Course".into(),
            session_date,
            start_time,
        }
    }

    // --- parse_decision ---

    #[test]
    fn parse_decision_accepts_approved() {
        // decide_approve_writes_attendance_leave_and_notification (tests/http_leave.rs)
        assert_eq!(parse_decision("approved").unwrap(), LeaveStatus::Approved);
    }

    #[test]
    fn parse_decision_accepts_rejected() {
        // decide_reject_does_not_write_attendance (tests/http_leave.rs)
        assert_eq!(parse_decision("rejected").unwrap(), LeaveStatus::Rejected);
    }

    #[test]
    fn parse_decision_rejects_pending_even_though_it_is_a_valid_leave_status() {
        // decide_invalid_status_value_returns_422 (tests/http_leave.rs): "pending"
        // is a valid LeaveStatus value but not one PATCH accepts.
        let err = parse_decision("pending").expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "status 僅接受 approved 或 rejected"),
            "got: {err:?}"
        );
    }

    #[test]
    fn parse_decision_rejects_garbage() {
        let err = parse_decision("bogus").expect_err("must reject");
        assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");
    }

    // --- check_decidable ---

    #[test]
    fn check_decidable_passes_for_pending_approve_with_active_enrolment() {
        // decide_approve_writes_attendance_leave_and_notification (tests/http_leave.rs)
        let ctx = decision_ctx(LeaveStatus::Pending, true);
        assert!(check_decidable(&ctx, LeaveStatus::Approved).is_ok());
    }

    #[test]
    fn check_decidable_allows_reject_with_cancelled_enrolment() {
        // decide_reject_cancelled_enrolment_succeeds (tests/http_leave.rs): a
        // cancelled enrolment blocks approval but not rejection.
        let ctx = decision_ctx(LeaveStatus::Pending, false);
        assert!(check_decidable(&ctx, LeaveStatus::Rejected).is_ok());
    }

    #[test]
    fn check_decidable_rejects_non_pending_as_409() {
        // decide_non_pending_returns_409 (tests/http_leave.rs)
        let ctx = decision_ctx(LeaveStatus::Approved, true);
        let err = check_decidable(&ctx, LeaveStatus::Rejected).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == DECIDE_NOT_PENDING),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_decidable_rejects_approve_with_cancelled_enrolment_as_409() {
        // decide_approve_cancelled_enrolment_returns_409 (tests/http_leave.rs)
        let ctx = decision_ctx(LeaveStatus::Pending, false);
        let err = check_decidable(&ctx, LeaveStatus::Approved).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == "報名已取消，無法核准請假"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_decidable_checks_not_pending_before_enrolment_cancelled() {
        // Error ordering: an already-decided request whose enrolment is also
        // cancelled must still surface the not-pending error, not the
        // enrolment one.
        let ctx = decision_ctx(LeaveStatus::Rejected, false);
        let err = check_decidable(&ctx, LeaveStatus::Approved).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == DECIDE_NOT_PENDING),
            "got: {err:?}"
        );
    }

    // --- check_makeup_source ---

    #[test]
    fn check_makeup_source_rejects_cancelled_enrolment_as_409() {
        // makeup_cancelled_enrolment_returns_409 (tests/http_leave.rs)
        let mut leave = leave(LeaveStatus::Approved, None);
        leave.enrolment_active = false;
        let err = check_makeup_source(&leave).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == "報名已取消，無法預約補課"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_source_checks_already_booked_before_enrolment_cancelled() {
        // Error ordering: an already-booked request whose enrolment is also
        // cancelled must still surface the already-booked error, not the
        // enrolment one.
        let mut leave = leave(LeaveStatus::Approved, Some(Uuid::now_v7()));
        leave.enrolment_active = false;
        let err = check_makeup_source(&leave).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == MAKEUP_ALREADY_BOOKED),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_source_passes_for_approved_unbooked_request() {
        // makeup_same_course_future_session_succeeds (tests/http_leave.rs)
        assert!(check_makeup_source(&leave(LeaveStatus::Approved, None)).is_ok());
    }

    #[test]
    fn check_makeup_source_rejects_non_approved_status_as_409() {
        // makeup_requires_approved_status_returns_409 (tests/http_leave.rs)
        let err = check_makeup_source(&leave(LeaveStatus::Pending, None)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == "僅已核准的假單可預約補課"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_source_rejects_already_booked_as_409() {
        // makeup_already_booked_returns_409 (tests/http_leave.rs)
        let err = check_makeup_source(&leave(LeaveStatus::Approved, Some(Uuid::now_v7())))
            .expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == MAKEUP_ALREADY_BOOKED),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_source_checks_status_before_already_booked() {
        // Error ordering: a non-approved request that also already carries a
        // makeup_session_id must still surface the status error, not the
        // already-booked one — mirrors service::book_makeup's original
        // inline check order.
        let err = check_makeup_source(&leave(LeaveStatus::Rejected, Some(Uuid::now_v7())))
            .expect_err("must reject");
        assert!(
            matches!(err, AppError::Conflict(ref m) if m == "僅已核准的假單可預約補課"),
            "got: {err:?}"
        );
    }

    // --- check_makeup_target ---

    #[test]
    fn check_makeup_target_passes_for_same_course_future_session() {
        // makeup_same_course_future_session_succeeds (tests/http_leave.rs)
        let course_id = Uuid::now_v7();
        let leave = leave(LeaveStatus::Approved, None);
        let leave = LeaveRequestForMakeup { course_id, ..leave };
        let target = session_context(course_id, d(2026, 7, 10), t(14, 0));
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 0, 0, 0).unwrap();
        assert!(check_makeup_target(&leave, &target, at(now)).is_ok());
    }

    #[test]
    fn check_makeup_target_rejects_different_course_as_422() {
        // makeup_target_session_different_course_returns_422 (tests/http_leave.rs)
        let leave = leave(LeaveStatus::Approved, None);
        let target = session_context(Uuid::now_v7(), d(2026, 7, 10), t(14, 0));
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 0, 0, 0).unwrap();
        let err = check_makeup_target(&leave, &target, at(now)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "補課場次須為同一課程"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_target_rejects_already_started_as_422() {
        // makeup_target_session_already_started_returns_422 (tests/http_leave.rs)
        let course_id = Uuid::now_v7();
        let leave = leave(LeaveStatus::Approved, None);
        let leave = LeaveRequestForMakeup { course_id, ..leave };
        let target = session_context(course_id, d(2026, 7, 5), t(9, 0));
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        let err = check_makeup_target(&leave, &target, at(now)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "補課場次已開始"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_target_rejects_dst_ambiguous_start_as_400() {
        // No direct http_leave.rs mirror (Asia/Taipei has no DST); mirrors
        // studio_clock's own America/New_York DST tests
        // (require_started_ambiguous_local_time_returns_bad_request) applied
        // through this rule's require_not_started call.
        let course_id = Uuid::now_v7();
        let leave = leave(LeaveStatus::Approved, None);
        let leave = LeaveRequestForMakeup { course_id, ..leave };
        // 2026-11-01 01:30 America/New_York occurs twice (clocks fall back).
        let target = session_context(course_id, d(2026, 11, 1), t(1, 30));
        let now = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        let ny_at = StudioNow {
            tz: "America/New_York".parse().expect("valid IANA name"),
            now,
        };
        let err = check_makeup_target(&leave, &target, ny_at).expect_err("must reject");
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "session time falls on an ambiguous local time"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_target_rejects_own_leave_session_as_422() {
        // makeup_into_own_leave_session_returns_422 (tests/http_leave.rs)
        let course_id = Uuid::now_v7();
        let leave = leave(LeaveStatus::Approved, None);
        let leave = LeaveRequestForMakeup { course_id, ..leave };
        let mut target = session_context(course_id, d(2026, 7, 10), t(14, 0));
        target.id = leave.session_id;
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 0, 0, 0).unwrap();
        let err = check_makeup_target(&leave, &target, at(now)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "補課場次不可為請假場次"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_target_checks_own_session_before_already_started() {
        // Error ordering: the leave's own session is also already-started
        // (it's in the past, being the session the member took leave for),
        // yet the own-session error must surface, not the already-started
        // one — mirrors service::book_makeup's original inline check order.
        let course_id = Uuid::now_v7();
        let leave = leave(LeaveStatus::Approved, None);
        let leave = LeaveRequestForMakeup { course_id, ..leave };
        let mut target = session_context(course_id, leave.session_date, leave.start_time);
        target.id = leave.session_id;
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        let err = check_makeup_target(&leave, &target, at(now)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "補課場次不可為請假場次"),
            "got: {err:?}"
        );
    }

    #[test]
    fn check_makeup_target_checks_course_before_already_started() {
        // Error ordering: a different-course target that has also already
        // started must still surface the course-mismatch error, not the
        // already-started one — mirrors service::book_makeup's original
        // inline check order.
        let leave = leave(LeaveStatus::Approved, None);
        let target = session_context(Uuid::now_v7(), d(2026, 7, 5), t(9, 0));
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        let err = check_makeup_target(&leave, &target, at(now)).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "補課場次須為同一課程"),
            "got: {err:?}"
        );
    }
}
