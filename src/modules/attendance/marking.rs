//! 點名計畫 (Marking Plan) — the two validation passes of
//! `PUT /sessions/{id}/attendance`'s bulk upsert, pulled out of the service
//! body into two pure functions: [`parse`] turns each record's raw `status`
//! string into an [`AttendanceStatus`] (422 on the first invalid value),
//! [`plan`] checks the parsed batch's enrolment ids against the caller-
//! resolved active/course-owned set (422 on any mismatch, contract §3.19
//! 裁決 2). Same shape as `orders::pricing`/`orders::fulfilment`: pure
//! function, zero DB, zero async — `service::bulk_upsert_attendance` still
//! owns everything genuinely transactional: session/coach lookup (404/403),
//! the studio-clock "already started" gate (422, contract §3.19 裁決 4),
//! the `repository::find_active_enrolment_ids_in` DB round trip (skipped
//! when the batch is empty), the upsert transaction loop, and the roster
//! re-read.
//!
//! **Error ordering is load-bearing: [`parse`] must run before the
//! enrolment-id DB query.** Today an invalid `status` string never triggers
//! that query at all — every record is parsed first, and only a
//! fully-parsed batch's enrolment ids get looked up. Moving the parse after
//! the query would let a DB outage turn a client-side 422 into a 500 for a
//! request that was invalid anyway.

use std::collections::HashSet;

use uuid::Uuid;

use crate::error::AppError;

use super::dto::AttendanceRecordEntry;
use super::model::AttendanceStatus;

/// A `PUT /sessions/{id}/attendance` batch, fully validated and ready for
/// `service` to write: `(enrolment_id, status)` pairs in the caller's
/// original order. Mirrors `orders::fulfilment::FulfilmentPlan`'s "owned,
/// pre-shaped output" role.
#[derive(Debug)]
pub struct MarkingPlan {
    pub entries: Vec<(Uuid, AttendanceStatus)>,
}

/// Parse every record's raw `status` string to an [`AttendanceStatus`].
/// Fails on the first invalid value (422, `invalid attendance status:
/// {status}`) — must run before any enrolment-id DB lookup, see the module
/// doc.
pub fn parse(
    records: &[AttendanceRecordEntry],
) -> Result<Vec<(Uuid, AttendanceStatus)>, AppError> {
    let mut parsed = Vec::with_capacity(records.len());
    for r in records {
        let status: AttendanceStatus = r.status.parse().map_err(|_| {
            AppError::Validation(format!("invalid attendance status: {}", r.status))
        })?;
        parsed.push((r.enrolment_id, status));
    }
    Ok(parsed)
}

/// Check the parsed batch's enrolment ids against `valid_enrolment_ids` —
/// the subset the caller already resolved (via
/// `repository::find_active_enrolment_ids_in`) to belong to this session's
/// course and be active. Any mismatch — a requested id missing from the
/// valid set (cross-course, cancelled, or nonexistent all look identical
/// here) — rejects the whole batch (422). This pure seam deliberately can't
/// and doesn't distinguish *why* an id is invalid; that finer-grained
/// distinction belongs to the `active_enrolments` view and the existing
/// http integration tests, not to this function's unit tests.
pub fn plan(
    parsed: Vec<(Uuid, AttendanceStatus)>,
    valid_enrolment_ids: &HashSet<Uuid>,
) -> Result<MarkingPlan, AppError> {
    let requested: HashSet<Uuid> = parsed.iter().map(|(id, _)| *id).collect();
    if requested != *valid_enrolment_ids {
        return Err(AppError::Validation(
            "all enrolments must belong to this session's course and be active".into(),
        ));
    }
    Ok(MarkingPlan { entries: parsed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(enrolment_id: Uuid, status: &str) -> AttendanceRecordEntry {
        AttendanceRecordEntry {
            enrolment_id,
            status: status.to_string(),
        }
    }

    // --- parse ---

    #[test]
    fn parse_accepts_all_three_statuses() {
        let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let records = [entry(a, "present"), entry(b, "absent"), entry(c, "leave")];
        let parsed = parse(&records).expect("parses");
        assert_eq!(
            parsed,
            vec![
                (a, AttendanceStatus::Present),
                (b, AttendanceStatus::Absent),
                (c, AttendanceStatus::Leave),
            ]
        );
    }

    #[test]
    fn parse_invalid_status_is_422() {
        // attendance_put_invalid_status_returns_422 (tests/http_attendance.rs)
        let records = [entry(Uuid::now_v7(), "late")];
        let err = parse(&records).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "invalid attendance status: late"),
            "got: {err:?}"
        );
    }

    #[test]
    fn parse_empty_batch_yields_empty_vec() {
        let parsed = parse(&[]).expect("parses");
        assert!(parsed.is_empty());
    }

    // --- plan ---

    #[test]
    fn plan_matching_sets_passes() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let parsed = vec![(a, AttendanceStatus::Present), (b, AttendanceStatus::Absent)];
        let valid: HashSet<Uuid> = [a, b].into_iter().collect();
        let plan = plan(parsed.clone(), &valid).expect("plans");
        assert_eq!(plan.entries, parsed);
    }

    #[test]
    fn plan_requested_id_missing_from_valid_set_is_422() {
        // attendance_put_cross_course_enrolment_rejects_whole_batch_with_no_writes
        // / attendance_put_cancelled_enrolment_rejects_whole_batch
        // (tests/http_attendance.rs): a requested id absent from the valid
        // set — whether cross-course, cancelled, or nonexistent — rejects
        // the whole batch. This seam can't and doesn't distinguish *why*.
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let parsed = vec![(a, AttendanceStatus::Present), (b, AttendanceStatus::Present)];
        let valid: HashSet<Uuid> = [a].into_iter().collect(); // b missing
        let err = plan(parsed, &valid).expect_err("must reject");
        assert!(
            matches!(
                err,
                AppError::Validation(ref m)
                    if m == "all enrolments must belong to this session's course and be active"
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn plan_valid_set_has_unrequested_id_is_also_422() {
        // Equality, not subset: the valid set carrying an id the batch never
        // asked about is just as much a mismatch as a missing id — guards
        // against a future `.is_subset()` simplification changing behavior.
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let parsed = vec![(a, AttendanceStatus::Present)];
        let valid: HashSet<Uuid> = [a, b].into_iter().collect();
        let err = plan(parsed, &valid).expect_err("must reject");
        assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");
    }

    #[test]
    fn plan_empty_batch_and_empty_valid_set_passes() {
        // service.rs's guard: an empty records batch skips the enrolment
        // query entirely and hands `plan` an empty valid set — the
        // vacuously-equal empty sets must still pass.
        let plan = plan(Vec::new(), &HashSet::new()).expect("plans");
        assert!(plan.entries.is_empty());
    }
}
