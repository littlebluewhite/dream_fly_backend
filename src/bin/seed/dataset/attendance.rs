//! Seed attendance: `mark_past_sessions` marks every active seed enrolment
//! on each already-materialized past session of its course — the seed's one
//! `INSERT INTO attendance_records`, a documented ADR-0010 bypass of
//! `attendance::records` (the runtime writer). Its rules, spelled out on
//! `mark_past_sessions`: the status follows a fixed formula over member and
//! session index; `marked_at` is the session's end, clamped to the run
//! instant; a session dated studio-local today is left unmarked; a row
//! already there, a coach's re-mark included, is kept on rerun.

use anyhow::Context;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::utils::studio_clock::StudioNow;

/// Mark every active enrolment on its course's sessions dated before
/// `at.today()` (studio-local), as `marked_by`. `active` is
/// `(member_idx, k, enrolment_id)`, `k` indexing `course_ids`. The status is
/// `(member_idx + session_idx) % 20` → 0-14 present / 15-17 absent / 18-19
/// leave (~75/15/10); members 22-24 are the low-attendance group → 0-7
/// present / 8-16 absent / 17-19 leave (~40/45/15), filling the
/// attendance-distribution report's low buckets. `marked_at` treats the
/// session's studio-local `end_time` as if it were already UTC (the same
/// shortcut `at_utc` takes), then clamps to `at.now` — the same clamp reason
/// as `at_utc`: a session dated yesterday studio-local can end, read that
/// way, after the run instant. Idempotent on `UNIQUE(session_id,
/// enrolment_id)`: a row already there — the seed's, or one a coach has
/// re-marked since — is kept. Returns how many rows the formula yields.
pub(super) async fn mark_past_sessions(
    db: &PgPool,
    at: StudioNow,
    marked_by: Uuid,
    course_ids: &[Uuid],
    active: &[(usize, usize, Uuid)],
) -> anyhow::Result<usize> {
    let today = at.today();
    let mut past_sessions: Vec<Vec<(Uuid, DateTime<Utc>)>> = Vec::with_capacity(course_ids.len());
    for course_id in course_ids {
        let rows: Vec<(Uuid, NaiveDate, NaiveTime)> = sqlx::query_as(
            "SELECT id, session_date, end_time FROM course_sessions \
             WHERE course_id = $1 AND session_date < $2 \
             ORDER BY session_date, start_time",
        )
        .bind(course_id)
        .bind(today)
        .fetch_all(db)
        .await
        .context("load past sessions for attendance")?;
        past_sessions.push(
            rows.into_iter()
                .map(|(id, d, t)| (id, d.and_time(t).and_utc().min(at.now)))
                .collect(),
        );
    }
    let mut attendance_rows: Vec<(Uuid, Uuid, AttendanceStatus, DateTime<Utc>)> = Vec::new();
    for (i, k, enrolment_id) in active {
        for (session_idx, (session_id, marked_at)) in past_sessions[*k].iter().enumerate() {
            let r = (i + session_idx) % 20;
            let status = if *i >= 22 {
                match r {
                    0..=7 => AttendanceStatus::Present,
                    8..=16 => AttendanceStatus::Absent,
                    _ => AttendanceStatus::Leave,
                }
            } else {
                match r {
                    0..=14 => AttendanceStatus::Present,
                    15..=17 => AttendanceStatus::Absent,
                    _ => AttendanceStatus::Leave,
                }
            };
            attendance_rows.push((*session_id, *enrolment_id, status, *marked_at));
        }
    }
    insert_attendance_bulk(db, marked_by, &attendance_rows).await?;
    Ok(attendance_rows.len())
}

/// Bulk-insert attendance rows (idempotent on the real
/// `UNIQUE(session_id, enrolment_id)` constraint). Single UNNEST statement,
/// mirroring the UNNEST bulk INSERT of `sessions::calendar`'s private
/// slot-date insert (behind `backfill_for_seed`).
async fn insert_attendance_bulk(
    db: &PgPool,
    marked_by: Uuid,
    rows: &[(Uuid, Uuid, AttendanceStatus, DateTime<Utc>)],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut session_ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut enrolment_ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut statuses: Vec<AttendanceStatus> = Vec::with_capacity(rows.len());
    let mut marked_ats: Vec<DateTime<Utc>> = Vec::with_capacity(rows.len());
    for (session_id, enrolment_id, status, marked_at) in rows {
        ids.push(Uuid::now_v7());
        session_ids.push(*session_id);
        enrolment_ids.push(*enrolment_id);
        statuses.push(*status);
        marked_ats.push(*marked_at);
    }

    sqlx::query(
        "INSERT INTO attendance_records (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         SELECT u.id, u.session_id, u.enrolment_id, u.status, $2, u.marked_at, NOW() \
         FROM UNNEST($1::uuid[], $3::uuid[], $4::uuid[], $5::attendance_status[], $6::timestamptz[]) \
              AS u(id, session_id, enrolment_id, status, marked_at) \
         ON CONFLICT (session_id, enrolment_id) DO NOTHING",
    )
    .bind(&ids)
    .bind(marked_by)
    .bind(&session_ids)
    .bind(&enrolment_ids)
    .bind(&statuses)
    .bind(&marked_ats)
    .execute(db)
    .await
    .context("bulk insert attendance_records")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::utils::studio_clock::StudioNow;

    use super::mark_past_sessions;
    use crate::dataset::{SeedAccount, upsert_account};

    /// The run instant every test here marks at: 2026-03-09T17:00Z, which is
    /// already 2026-03-10 01:00 in Asia/Taipei — the studio-local `today`
    /// runs a day ahead of the UTC date.
    fn at() -> StudioNow {
        StudioNow {
            tz: "Asia/Taipei".parse().expect("valid IANA name"),
            now: Utc.with_ymd_and_hms(2026, 3, 9, 17, 0, 0).unwrap(),
        }
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, day).unwrap()
    }

    /// The smallest catalog attendance is marked against: one admin who
    /// marks, one course and two active enrolments in it — reporting members
    /// 17 (regular group) and 23 (low-attendance group). The course meets
    /// 03-04 … 03-08 at 09:00-10:00, 03-09 at 20:00-21:00 (its end, read as
    /// UTC, is after `at().now`) and today, 03-10, at 09:00-10:00.
    struct Catalog {
        admin: Uuid,
        course: Uuid,
        member_17: Uuid,
        member_23: Uuid,
        today_session: Uuid,
    }

    async fn minimal_catalog(db: &PgPool) -> Catalog {
        let account = |email: &'static str| SeedAccount {
            email,
            name: "示範帳號",
            password_hash: "not-a-real-hash",
            birth_date: None,
            points: 0,
            joined_at: Utc.with_ymd_and_hms(2025, 1, 1, 4, 0, 0).unwrap(),
        };
        let mut ids = Vec::new();
        for email in [
            "admin@dreamfly.tw",
            "seed-member-17@dreamfly.tw",
            "seed-member-23@dreamfly.tw",
        ] {
            ids.push(
                upsert_account(db, &account(email), at().now)
                    .await
                    .expect("upsert account"),
            );
        }
        let course: Uuid = sqlx::query_scalar(
            "INSERT INTO courses (name, slug, level, duration_minutes, price_cents, max_students) \
             VALUES ('跑酷入門', 'parkour-intro', 'beginner'::course_level, 60, 50000, 12) \
             RETURNING id",
        )
        .fetch_one(db)
        .await
        .expect("insert course");
        // Business time bound, not left to the column DEFAULT (the DB clock):
        // enrolled before the course's first session, 03-04.
        let enrolled_at = Utc.with_ymd_and_hms(2026, 3, 1, 4, 0, 0).unwrap();
        let mut enrolments = Vec::new();
        for user in &ids[1..] {
            enrolments.push(
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO enrolments (user_id, course_id, enrolled_at, created_at) \
                     VALUES ($1, $2, $3, $3) RETURNING id",
                )
                .bind(user)
                .bind(course)
                .bind(enrolled_at)
                .fetch_one(db)
                .await
                .expect("insert enrolment"),
            );
        }
        let mut sessions = Vec::new();
        for (day, start, end) in [
            (4, "09:00", "10:00"),
            (5, "09:00", "10:00"),
            (6, "09:00", "10:00"),
            (7, "09:00", "10:00"),
            (8, "09:00", "10:00"),
            (9, "20:00", "21:00"),
            (10, "09:00", "10:00"),
        ] {
            sessions.push(
                sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO course_sessions (course_id, session_date, start_time, end_time) \
                     VALUES ($1, $2, $3::time, $4::time) RETURNING id",
                )
                .bind(course)
                .bind(date(day))
                .bind(start)
                .bind(end)
                .fetch_one(db)
                .await
                .expect("insert session"),
            );
        }
        Catalog {
            admin: ids[0],
            course,
            member_17: enrolments[0],
            member_23: enrolments[1],
            today_session: sessions[6],
        }
    }

    /// Mark the catalog's two enrolments as `run` does: member index, the
    /// course's index in `course_ids`, enrolment id.
    async fn mark(db: &PgPool, c: &Catalog) -> usize {
        mark_past_sessions(
            db,
            at(),
            c.admin,
            &[c.course],
            &[(17, 0, c.member_17), (23, 0, c.member_23)],
        )
        .await
        .expect("mark past sessions")
    }

    /// `(session_date, status, marked_at, marked_by)` of every attendance row
    /// of `enrolment`, oldest session first.
    async fn rows(db: &PgPool, enrolment: Uuid) -> Vec<(NaiveDate, String, DateTime<Utc>, Uuid)> {
        sqlx::query_as(
            "SELECT s.session_date, a.status::TEXT, a.marked_at, a.marked_by \
             FROM attendance_records a JOIN course_sessions s ON s.id = a.session_id \
             WHERE a.enrolment_id = $1 ORDER BY s.session_date, s.start_time",
        )
        .bind(enrolment)
        .fetch_all(db)
        .await
        .expect("fetch attendance rows")
    }

    fn statuses(rows: &[(NaiveDate, String, DateTime<Utc>, Uuid)]) -> Vec<&str> {
        rows.iter()
            .map(|(_, status, _, _)| status.as_str())
            .collect()
    }

    /// Each active enrolment is marked on every past session of its course,
    /// by `marked_by`, with the status `(member_idx + session_idx) % 20`
    /// picks — the low-attendance group (members 22-24) on its own scale.
    #[sqlx::test]
    async fn marks_past_sessions_by_the_formula(db: PgPool) {
        let c = minimal_catalog(&db).await;

        assert_eq!(mark(&db, &c).await, 12);

        let member_17 = rows(&db, c.member_17).await;
        assert_eq!(
            statuses(&member_17),
            ["absent", "leave", "leave", "present", "present", "present"]
        );
        let member_23 = rows(&db, c.member_23).await;
        assert_eq!(
            statuses(&member_23),
            [
                "present", "present", "present", "present", "present", "absent"
            ]
        );
        assert!(
            member_17
                .iter()
                .chain(&member_23)
                .all(|(_, _, _, by)| *by == c.admin)
        );
    }

    /// `marked_at` is the session's end read as UTC, clamped to the run
    /// instant: 03-09's 21:00 end is after `at().now` (17:00Z), so it is
    /// marked at `at().now`.
    #[sqlx::test]
    async fn marked_at_is_the_session_end_clamped_to_now(db: PgPool) {
        let c = minimal_catalog(&db).await;
        mark(&db, &c).await;

        let marked_at: Vec<DateTime<Utc>> = rows(&db, c.member_17)
            .await
            .into_iter()
            .map(|(_, _, marked_at, _)| marked_at)
            .collect();
        let end = |day| Utc.with_ymd_and_hms(2026, 3, day, 10, 0, 0).unwrap();
        assert_eq!(
            marked_at,
            [end(4), end(5), end(6), end(7), end(8), at().now]
        );
    }

    /// A session dated studio-local today (03-10) is not marked; it is the
    /// studio-local date, not `at().now`'s UTC one (03-09), that decides.
    #[sqlx::test]
    async fn todays_session_is_not_marked(db: PgPool) {
        let c = minimal_catalog(&db).await;
        mark(&db, &c).await;

        let marked: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM attendance_records WHERE session_id = $1")
                .bind(c.today_session)
                .fetch_one(&db)
                .await
                .expect("count today's rows");
        assert_eq!(marked, 0);
    }

    /// Drift: once a coach has re-marked a seeded row, a rerun keeps the
    /// coach's status and adds no row.
    #[sqlx::test]
    async fn rerun_keeps_a_re_marked_result(db: PgPool) {
        let c = minimal_catalog(&db).await;
        mark(&db, &c).await;
        sqlx::query(
            "UPDATE attendance_records SET status = 'present' \
             WHERE enrolment_id = $1 AND status = 'absent'",
        )
        .bind(c.member_17)
        .execute(&db)
        .await
        .expect("re-mark member 17's absence");

        assert_eq!(mark(&db, &c).await, 12);

        assert_eq!(
            statuses(&rows(&db, c.member_17).await),
            ["present", "leave", "leave", "present", "present", "present"]
        );
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attendance_records")
            .fetch_one(&db)
            .await
            .expect("count rows");
        assert_eq!(total, 12);
    }
}
