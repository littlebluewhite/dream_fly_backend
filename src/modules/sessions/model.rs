use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use uuid::Uuid;

use crate::utils::studio_clock;

/// A course's structured weekly meeting pattern — one row per (day_of_week,
/// start_time). Mirrors `coach_schedules`' shape. `day_of_week` is 0=Sunday
/// .. 6=Saturday (PostgreSQL `EXTRACT(DOW)` convention — see
/// `repository::materialize_range`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseScheduleSlot {
    pub id: Uuid,
    pub course_id: Uuid,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A session's back-end-derived lifecycle stage — see [`SessionStatus::derive`].
/// Not a database column and not a state machine: every read recomputes it
/// from the current wall-clock time, so it can never go stale or need a
/// migration when the studio's schedule changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Upcoming,
    Ongoing,
    Done,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Upcoming => "upcoming",
            Self::Ongoing => "ongoing",
            Self::Done => "done",
        }
    }

    /// Derive a session's lifecycle stage from the studio-local wall clock.
    /// Boundary semantics are **[start, end) 閉開**: `now == start_time` is
    /// already `Ongoing`, `now == end_time` is already `Done` — matching
    /// `studio_clock::has_started`'s existing "at or after" convention so
    /// the three states tile the timeline with no gap and no overlap.
    ///
    /// `has_started`/`has_ended` return `None` on a DST-ambiguous or
    /// nonexistent local time (see `studio_clock::to_utc`); Asia/Taipei (the
    /// only timezone this runs under in production) has no DST, so that
    /// branch is unreachable there — it exists for correctness under any
    /// configured timezone, and is tested against America/New_York. On
    /// ambiguity this falls back to comparing `session_date` against the
    /// studio-local calendar date (`today`) alone: a date-level answer is
    /// still useful even when the instant-level one is undefined.
    pub fn derive(
        tz: Tz,
        now: DateTime<Utc>,
        session_date: NaiveDate,
        start_time: NaiveTime,
        end_time: NaiveTime,
    ) -> SessionStatus {
        let started = studio_clock::has_started(tz, now, session_date, start_time);
        let ended = studio_clock::has_ended(tz, now, session_date, end_time);

        match (started, ended) {
            (Some(false), _) => SessionStatus::Upcoming,
            (Some(true), Some(false)) => SessionStatus::Ongoing,
            (Some(true), Some(true)) => SessionStatus::Done,
            _ => {
                let today = studio_clock::today(tz, now);
                if session_date < today {
                    SessionStatus::Done
                } else if session_date > today {
                    SessionStatus::Upcoming
                } else if started == Some(true) {
                    SessionStatus::Ongoing
                } else {
                    SessionStatus::Upcoming
                }
            }
        }
    }
}

/// A materialized calendar-date occurrence of a `CourseScheduleSlot`. No
/// `status` column — v1 has no course-suspension feature; the
/// upcoming/ongoing/done lifecycle is derived by [`SessionStatus::derive`]
/// and carried on the DTO, not stored on this row.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CourseSession {
    pub id: Uuid,
    pub course_id: Uuid,
    pub session_date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub created_at: DateTime<Utc>,
}

/// One row of `GET /sessions/today` — a materialized session JOINed with its
/// course name, plus a live count of that course's active enrolments (same
/// correlated-subquery pattern as `courses::model::Course::enrolled_count`).
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct TodaySessionRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub enrolled_count: i64,
}

/// One row of `GET /schedule/me` — a course's weekly slot (not a materialized
/// session) JOINed with its course name and coach name. `coach_name` is
/// `None` when the course has no assigned coach (`courses.coach_id` is
/// nullable).
#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct MyScheduleRow {
    pub course_id: Uuid,
    pub course_name: String,
    pub coach_name: Option<String>,
    pub day_of_week: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn new_york() -> Tz {
        "America/New_York".parse::<Tz>().expect("valid IANA name")
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn derive_utc_boundary_table() {
        let date = d(2026, 7, 5);
        let start = t(9, 0);
        let end = t(10, 0);

        let cases: [(&str, DateTime<Utc>, SessionStatus); 5] = [
            (
                "before start -> upcoming",
                Utc.with_ymd_and_hms(2026, 7, 5, 8, 59, 59).unwrap(),
                SessionStatus::Upcoming,
            ),
            (
                "at start boundary -> ongoing",
                Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap(),
                SessionStatus::Ongoing,
            ),
            (
                "mid-session -> ongoing",
                Utc.with_ymd_and_hms(2026, 7, 5, 9, 30, 0).unwrap(),
                SessionStatus::Ongoing,
            ),
            (
                "at end boundary -> done",
                Utc.with_ymd_and_hms(2026, 7, 5, 10, 0, 0).unwrap(),
                SessionStatus::Done,
            ),
            (
                "after end -> done",
                Utc.with_ymd_and_hms(2026, 7, 5, 10, 0, 1).unwrap(),
                SessionStatus::Done,
            ),
        ];

        for (label, now, expected) in cases {
            assert_eq!(
                SessionStatus::derive(chrono_tz::UTC, now, date, start, end),
                expected,
                "case: {label}"
            );
        }
    }

    struct DstFallbackCase {
        label: &'static str,
        now: DateTime<Utc>,
        date: NaiveDate,
        start: NaiveTime,
        end: NaiveTime,
        expected: SessionStatus,
    }

    #[test]
    fn derive_new_york_dst_fallback_table() {
        let cases = [
            DstFallbackCase {
                label: "fall-back ambiguous start; session date before today -> done",
                now: Utc.with_ymd_and_hms(2026, 11, 5, 17, 0, 0).unwrap(),
                date: d(2026, 11, 1),
                start: t(1, 30),
                end: t(3, 0),
                expected: SessionStatus::Done,
            },
            DstFallbackCase {
                label: "spring-forward nonexistent start; session date after today -> upcoming",
                now: Utc.with_ymd_and_hms(2026, 3, 1, 17, 0, 0).unwrap(),
                date: d(2026, 3, 8),
                start: t(2, 30),
                end: t(4, 0),
                expected: SessionStatus::Upcoming,
            },
            DstFallbackCase {
                label: "spring-forward nonexistent end; same day, already started -> ongoing",
                now: Utc.with_ymd_and_hms(2026, 3, 8, 6, 30, 0).unwrap(),
                date: d(2026, 3, 8),
                start: t(1, 0),
                end: t(2, 30),
                expected: SessionStatus::Ongoing,
            },
            DstFallbackCase {
                label: "spring-forward nonexistent start; same day, not resolved-started -> upcoming",
                now: Utc.with_ymd_and_hms(2026, 3, 8, 19, 0, 0).unwrap(),
                date: d(2026, 3, 8),
                start: t(2, 30),
                end: t(4, 0),
                expected: SessionStatus::Upcoming,
            },
        ];

        for case in cases {
            assert_eq!(
                SessionStatus::derive(new_york(), case.now, case.date, case.start, case.end),
                case.expected,
                "case: {}",
                case.label
            );
        }
    }
}
