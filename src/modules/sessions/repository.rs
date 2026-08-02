use chrono::{NaiveDate, NaiveTime};
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CourseSession, MyScheduleRow, TodaySessionRow};

/// Proof that `materialize_range(db, course_ids, from, to)` has already run
/// for this exact `(course_ids, from, to)` — collapses the "materialize
/// then read" call-order invariant, previously enforced only by doc comments
/// across 5 call sites, into the type system: read functions take `&
/// MaterializedRange` instead of raw `(course_ids, from, to)` parameters.
///
/// This is **not** a course-scope filter guarantee: it proves the range was
/// materialized, not that every reader filters by `course_ids`. Some readers
/// (e.g. `reports::repository::venue_usage`) use only the date window and
/// ignore `course_ids` entirely — see each reader's own doc for its actual
/// scope. Fields are private; only `materialize_range` can construct one.
///
/// 姊妹型別 [`MaterializedDay`]:額外要求單日(`from == to`)的消費端改收
/// 它——不再靠讀取端自行斷言 `from_date() == to_date()`,分工細節見它自己
/// 的 doc。
#[derive(Debug, Clone)]
pub struct MaterializedRange {
    course_ids: Vec<Uuid>,
    from: NaiveDate,
    to: NaiveDate,
}

impl MaterializedRange {
    pub fn course_ids(&self) -> &[Uuid] {
        &self.course_ids
    }

    /// Named `from_date` (not `from`) to avoid colliding with
    /// `std::convert::From`.
    pub fn from_date(&self) -> NaiveDate {
        self.from
    }

    pub fn to_date(&self) -> NaiveDate {
        self.to
    }
}

/// 證明 `materialize_day(db, course_ids, date)` 已針對這組確切的
/// `(course_ids, date)` 執行過——[`MaterializedRange`] 的單日姊妹型別。
/// 兩個消費端額外要求單日窗(`from == to`):`find_today_sessions_in`——
/// `TodaySessionRow` 無日期欄,多日範圍會把多天混成「今天」;
/// `coach_today_and_pending`——「今天」本身無多日語意。這個前提以前只靠
/// 讀取端自行 `debug_assert!(from_date() == to_date())` 把關,是唯一防線:
/// `MaterializedRange` 本身允許任意區間,沒有任何上游 owner 保證這次呼叫
/// 只物化了一天,release build 拿掉這道斷言後,悄悄傳入的多日 witness 沒
/// 有其他防線接住(對照 `LedgerDelta` 幅度 debug_assert 的
/// defense-in-depth 判準,見 ADR-0007 第四則 Addendum)。本型別把「單日」
/// 前提收進建構點:[`materialize_day`] 是唯一建構方式,消費端不再需要自
/// 行斷言。
///
/// 與 [`MaterializedRange`] 分工:只要求「這個範圍已物化」的讀取端繼續
/// 收 `&MaterializedRange`;額外要求「而且剛好一天」的讀取端改收
/// `&MaterializedDay`。欄位全私有,僅 `materialize_day` 能建構。
#[derive(Debug, Clone)]
pub struct MaterializedDay {
    range: MaterializedRange,
}

impl MaterializedDay {
    pub fn course_ids(&self) -> &[Uuid] {
        self.range.course_ids()
    }

    pub fn date(&self) -> NaiveDate {
        self.range.from_date()
    }
}

/// Materialize `course_sessions` rows for every date in `[from, to]` whose
/// weekday matches one of `course_ids`' weekly slots. Idempotent — calling
/// this twice for the same range never creates duplicate rows, thanks to
/// `ON CONFLICT DO NOTHING` on `course_sessions_unique`. Returns a
/// [`MaterializedRange`] witness for this exact `(course_ids, from, to)` —
/// including on both early-return paths — so callers thread it into the
/// matching read function instead of re-stating the "materialize first"
/// precondition in prose.
///
/// Implemented as two steps (candidate SELECT, then a Rust-id-keyed bulk
/// INSERT via UNNEST) rather than a single `INSERT ... SELECT` so that every
/// row's `id` is a `Uuid::now_v7()` generated in application code, per this
/// repo's ID convention — mirrors `schedule::repository::bulk_create_tx`'s
/// UNNEST + `ARRAY_FILL` shape for the constant `created_at` column.
pub async fn materialize_range(
    db: &PgPool,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<MaterializedRange, sqlx::Error> {
    let witness = MaterializedRange { course_ids: course_ids.to_vec(), from, to };

    if course_ids.is_empty() {
        return Ok(witness);
    }

    let candidates = sqlx::query_as::<_, (Uuid, NaiveDate, NaiveTime, NaiveTime)>(
        "SELECT s.course_id, gs.d::date, s.start_time, s.end_time \
         FROM generate_series($2::date, $3::date, interval '1 day') AS gs(d) \
         JOIN course_schedule_slots s \
           ON s.course_id = ANY($1::uuid[]) \
          AND s.day_of_week = EXTRACT(DOW FROM gs.d)::smallint",
    )
    .bind(course_ids)
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await?;

    if candidates.is_empty() {
        return Ok(witness);
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(candidates.len());
    let mut c_ids: Vec<Uuid> = Vec::with_capacity(candidates.len());
    let mut dates: Vec<NaiveDate> = Vec::with_capacity(candidates.len());
    let mut starts: Vec<NaiveTime> = Vec::with_capacity(candidates.len());
    let mut ends: Vec<NaiveTime> = Vec::with_capacity(candidates.len());

    for (course_id, session_date, start_time, end_time) in &candidates {
        ids.push(Uuid::now_v7());
        c_ids.push(*course_id);
        dates.push(*session_date);
        starts.push(*start_time);
        ends.push(*end_time);
    }

    sqlx::query(
        "INSERT INTO course_sessions (id, course_id, session_date, start_time, end_time, created_at) \
         SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::date[], $4::time[], $5::time[], \
         ARRAY_FILL(now(), ARRAY[$6::int])::timestamptz[]) \
         ON CONFLICT (course_id, session_date, start_time) DO NOTHING",
    )
    .bind(&ids)
    .bind(&c_ids)
    .bind(&dates)
    .bind(&starts)
    .bind(&ends)
    .bind(candidates.len() as i32)
    .execute(db)
    .await?;

    Ok(witness)
}

/// `materialize_day` 是 [`MaterializedDay`] 的唯一建構點——內部呼叫
/// `materialize_range(db, course_ids, date, date)`,冪等與早退邏輯全部
/// 重用,不重複實作。
pub async fn materialize_day(
    db: &PgPool,
    course_ids: &[Uuid],
    date: NaiveDate,
) -> Result<MaterializedDay, sqlx::Error> {
    let range = materialize_range(db, course_ids, date, date).await?;
    Ok(MaterializedDay { range })
}

pub async fn find_sessions_in(
    db: &PgPool,
    mat: &MaterializedRange,
) -> Result<Vec<CourseSession>, sqlx::Error> {
    sqlx::query_as::<_, CourseSession>(
        "SELECT id, course_id, session_date, start_time, end_time, created_at \
         FROM course_sessions \
         WHERE course_id = ANY($1::uuid[]) AND session_date BETWEEN $2 AND $3 \
         ORDER BY session_date, start_time",
    )
    .bind(mat.course_ids())
    .bind(mat.from_date())
    .bind(mat.to_date())
    .fetch_all(db)
    .await
}

/// All course ids — the materialize/query scope for an admin's
/// `GET /sessions/today`. A plain `SELECT id FROM courses` naming the table
/// directly (not going through `courses::repository`) mirrors the existing
/// cross-module JOIN convention (e.g. `enrolments::repository` joins
/// `courses` the same way).
pub async fn find_all_course_ids(db: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM courses").fetch_all(db).await
}

/// Course ids owned by a given coach — the materialize/query scope for a
/// coach's own `GET /sessions/today`.
pub async fn find_course_ids_by_coach(db: &PgPool, coach_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM courses WHERE coach_id = $1")
        .bind(coach_id)
        .fetch_all(db)
        .await
}

/// `coach_name` JOINs the same way as `find_my_weekly_schedule` (courses ->
/// coaches -> users, LEFT so a coachless course still yields a row).
/// `venue` rejoins `course_schedule_slots` on the session's derived
/// `(course_id, day_of_week, start_time)` — the reversible key
/// `course_schedule_slots_unique` guarantees at most one match, so this
/// LEFT JOIN can never fan out a session into more than one row.
///
/// 收 [`MaterializedDay`]——單日前提已在型別層成立,不再需要在此自行
/// 斷言。
pub async fn find_today_sessions_in(
    db: &PgPool,
    day: &MaterializedDay,
) -> Result<Vec<TodaySessionRow>, sqlx::Error> {
    if day.course_ids().is_empty() {
        return Ok(Vec::new());
    }

    // enrolled_count:座位 COUNT 謂詞的顯示用 inline 拷貝——owner 是 `active_enrolments` view(migration `20260711000001`),非 `courses::seats`(見其模組 doc)。
    sqlx::query_as::<_, TodaySessionRow>(
        "SELECT cs.id, cs.course_id, c.name AS course_name, u.name AS coach_name, \
         cs.start_time, cs.end_time, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = cs.course_id) AS enrolled_count, \
         s.venue AS venue \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         LEFT JOIN coaches co ON co.id = c.coach_id \
         LEFT JOIN users u ON u.id = co.user_id \
         LEFT JOIN course_schedule_slots s \
           ON s.course_id = cs.course_id \
          AND s.day_of_week = EXTRACT(DOW FROM cs.session_date)::smallint \
          AND s.start_time = cs.start_time \
         WHERE cs.session_date = $1 AND cs.course_id = ANY($2::uuid[]) \
         ORDER BY cs.start_time",
    )
    .bind(day.date())
    .bind(day.course_ids())
    .fetch_all(db)
    .await
}

/// The caller's weekly schedule: every schedule slot belonging to a course
/// they hold an *active* enrolment in. Not materialized — a direct read of
/// the weekly pattern, per the task brief.
pub async fn find_my_weekly_schedule(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyScheduleRow>, sqlx::Error> {
    sqlx::query_as::<_, MyScheduleRow>(
        "SELECT c.id AS course_id, c.name AS course_name, u.name AS coach_name, \
         s.day_of_week, s.start_time, s.end_time, s.venue \
         FROM active_enrolments e \
         JOIN courses c ON c.id = e.course_id \
         JOIN course_schedule_slots s ON s.course_id = c.id \
         LEFT JOIN coaches co ON co.id = c.coach_id \
         LEFT JOIN users u ON u.id = co.user_id \
         WHERE e.user_id = $1 \
         ORDER BY s.day_of_week, s.start_time",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}
