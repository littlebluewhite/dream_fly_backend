//! 場次日曆:`course_sessions` 與 `course_schedule_slots` 兩張表的唯一
//! runtime 寫入者。
//!
//! - 物化:[`materialize_range`]/[`materialize_today`] 把週課表展開成場次列,
//!   只往前(≥ studio 今天,ADR-0011),並回傳
//!   [`MaterializedRange`]/[`MaterializedDay`] witness 給讀取端;seed 專用的
//!   [`backfill_for_seed`] 是唯一可寫過去日期的入口。
//! - 週課表:[`set_initial_schedule_tx`](`create_course`)與
//!   [`replace_weekly_schedule_tx`](換 slot + 對齊未來場次)。
//!
//! 「哪個 slot 對應哪個場次」的規則只寫一次:[`SLOT_OF_SESSION`],物化的候
//! 選 SELECT 與對齊的 UPDATE/DELETE 三處逐字套用同一片段(fragment const +
//! `format!`,前例:`courses::repository::COURSE_COLUMNS`)。不用 view:物
//! 化當下場次列還不存在,view 只能涵蓋三處中的兩處。migration
//! `20260927000001` 的 backfill 抄本屬於歷史,不改。
//!
//! 只擁有寫入與 witness;讀取(`sessions::repository::find_sessions_in`、
//! `reports::repository::venue_usage` 等)照「跨模組讀表」慣例留在各自模組,
//! 只從這裡 import witness 型別。型別 `CourseScheduleSlot` 與讀取
//! `find_slots_by_course` 仍歸 courses。前例:`bookings::occupancy` 把
//! `time_slots.booked` 的寫入從 schedule 收走。

use chrono::{NaiveDate, NaiveTime};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// slot `s` 與場次 `cs` 的對應規則:同課程、場次日期的星期 = slot 的
/// `day_of_week`(`EXTRACT(DOW)`,0=Sunday..6=Saturday)、同開始時間。套用端
/// 必須在 scope 內提供別名 `s`(`course_schedule_slots`)與
/// `cs(course_id, session_date, start_time)`。
const SLOT_OF_SESSION: &str = "s.course_id = cs.course_id \
  AND s.day_of_week = EXTRACT(DOW FROM cs.session_date)::smallint AND s.start_time = cs.start_time";

/// `(day_of_week, start_time, end_time, venue)` — pre-parsed input row for
/// [`set_initial_schedule_tx`]/[`replace_weekly_schedule_tx`] (parsed and
/// validated by `courses::service`). Aliased for readability, mirroring
/// `schedule::repository::SlotRow`.
pub type SlotRow = (i16, NaiveTime, NaiveTime, Option<String>);

/// Proof that `materialize_range(db, today, course_ids, from, to)` has
/// already run for this exact `(course_ids, from, to)` — collapses the
/// "materialize then read" call-order invariant, previously enforced only by
/// doc comments across 5 call sites, into the type system: read functions
/// take `&MaterializedRange` instead of raw `(course_ids, from, to)`
/// parameters. The bounds are the *requested* window; only its
/// `≥ today` part was materialized, dates before that are whatever rows
/// already exist (ADR-0011) — readers need not care which.
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

/// 證明 `materialize_today(db, course_ids, today)` 已針對這組確切的
/// `(course_ids, today)` 執行過——[`MaterializedRange`] 的單日姊妹型別。
/// 兩個消費端額外要求單日窗(`from == to`):`find_today_sessions_in`——
/// `TodaySessionRow` 無日期欄,多日範圍會把多天混成「今天」;
/// `coach_today_and_pending`——「今天」本身無多日語意。這個前提以前只靠
/// 讀取端自行 `debug_assert!(from_date() == to_date())` 把關,是唯一防線:
/// `MaterializedRange` 本身允許任意區間,沒有任何上游 owner 保證這次呼叫
/// 只物化了一天,release build 拿掉這道斷言後,悄悄傳入的多日 witness 沒
/// 有其他防線接住(對照 `LedgerDelta` 幅度 debug_assert 的
/// defense-in-depth 判準,見 ADR-0007 第四則 Addendum)。本型別把「單日」
/// 前提收進建構點:[`materialize_today`] 是唯一建構方式,消費端不再需要自
/// 行斷言。
///
/// 與 [`MaterializedRange`] 分工:只要求「這個範圍已物化」的讀取端繼續
/// 收 `&MaterializedRange`;額外要求「而且剛好一天」的讀取端改收
/// `&MaterializedDay`。欄位全私有,僅 `materialize_today` 能建構。
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

/// Materialize `course_sessions` rows for every date in
/// `[max(from, today), to]` whose weekday matches one of `course_ids`' weekly
/// slots — **forward only** (ADR-0011): a date before `today` (the
/// studio-local calendar date) is never created, because today's weekly
/// schedule says nothing reliable about what actually ran on a past date.
/// Past dates in `[from, to]` are read from whatever rows already exist.
/// Idempotent — calling this twice for the same range never creates
/// duplicate rows, thanks to `ON CONFLICT DO NOTHING` on
/// `course_sessions_unique`. Each new row snapshots its slot's `venue` — an
/// already-materialized session keeps the venue it was created with (the
/// conflict skips it); only [`replace_weekly_schedule_tx`] re-syncs it, and
/// only for future dates.
///
/// Returns a [`MaterializedRange`] witness for the *requested*
/// `(course_ids, from, to)` — not the clamped window, and including on every
/// early-return path (no courses, or a range entirely before `today`) — so
/// callers thread it into the matching read function unchanged instead of
/// re-stating the "materialize first" precondition in prose.
pub async fn materialize_range(
    db: &PgPool,
    today: NaiveDate,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<MaterializedRange, sqlx::Error> {
    let witness = MaterializedRange {
        course_ids: course_ids.to_vec(),
        from,
        to,
    };

    let start = from.max(today);
    if start > to {
        return Ok(witness);
    }
    insert_sessions(db, course_ids, start, to).await?;

    Ok(witness)
}

/// `materialize_today` 是 [`MaterializedDay`] 的唯一建構點——內部呼叫
/// `materialize_range(db, today, course_ids, today, today)`,冪等與早退邏
/// 輯全部重用,不重複實作。
pub async fn materialize_today(
    db: &PgPool,
    course_ids: &[Uuid],
    today: NaiveDate,
) -> Result<MaterializedDay, sqlx::Error> {
    let range = materialize_range(db, today, course_ids, today, today).await?;
    Ok(MaterializedDay { range })
}

/// **Seed only** (`src/bin/seed/dataset.rs`) — runtime code must never call this.
/// Fills every date in `[from, to]`, past dates included, from the
/// *current* weekly schedule: exactly the phantom-session source
/// [`materialize_range`] forbids at runtime (ADR-0011), acceptable only for
/// a fresh dev dataset whose schedule has never changed. Same idempotent
/// INSERT as `materialize_range`; returns no witness because no reader
/// should consume its output directly.
pub async fn backfill_for_seed(
    db: &PgPool,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(), sqlx::Error> {
    insert_sessions(db, course_ids, from, to).await
}

/// The shared INSERT behind [`materialize_range`] and
/// [`backfill_for_seed`]: create every slot date in `[from, to]` for
/// `course_ids` that doesn't exist yet. No date policy of its own — callers
/// decide the window.
///
/// Implemented as two steps (candidate SELECT, then a Rust-id-keyed bulk
/// INSERT via UNNEST) rather than a single `INSERT ... SELECT` so that every
/// row's `id` is a `Uuid::now_v7()` generated in application code, per this
/// repo's ID convention — mirrors `schedule::repository::bulk_create_tx`'s
/// UNNEST + `ARRAY_FILL` shape for the constant `created_at` column. The
/// candidate SELECT derives each would-be session `cs` from slot × date and
/// keeps it only when [`SLOT_OF_SESSION`] holds — the same rule the
/// reconcile UPDATE/DELETE apply to existing rows.
async fn insert_sessions(
    db: &PgPool,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(), sqlx::Error> {
    if course_ids.is_empty() {
        return Ok(());
    }

    let candidates = sqlx::query_as::<_, (Uuid, NaiveDate, NaiveTime, NaiveTime, Option<String>)>(
        sqlx::AssertSqlSafe(format!(
            "SELECT s.course_id, cs.session_date, s.start_time, s.end_time, s.venue \
             FROM course_schedule_slots s \
             CROSS JOIN generate_series($2::date, $3::date, interval '1 day') AS gs(d) \
             CROSS JOIN LATERAL (SELECT s.course_id, gs.d::date, s.start_time) \
               AS cs(course_id, session_date, start_time) \
             WHERE s.course_id = ANY($1::uuid[]) \
               AND {SLOT_OF_SESSION}"
        )),
    )
    .bind(course_ids)
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await?;

    if candidates.is_empty() {
        return Ok(());
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(candidates.len());
    let mut c_ids: Vec<Uuid> = Vec::with_capacity(candidates.len());
    let mut dates: Vec<NaiveDate> = Vec::with_capacity(candidates.len());
    let mut starts: Vec<NaiveTime> = Vec::with_capacity(candidates.len());
    let mut ends: Vec<NaiveTime> = Vec::with_capacity(candidates.len());
    let mut venues: Vec<Option<String>> = Vec::with_capacity(candidates.len());

    for (course_id, session_date, start_time, end_time, venue) in &candidates {
        ids.push(Uuid::now_v7());
        c_ids.push(*course_id);
        dates.push(*session_date);
        starts.push(*start_time);
        ends.push(*end_time);
        venues.push(venue.clone());
    }

    sqlx::query(
        "INSERT INTO course_sessions (id, course_id, session_date, start_time, end_time, venue, created_at) \
         SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::date[], $4::time[], $5::time[], $6::text[], \
         ARRAY_FILL(now(), ARRAY[$7::int])::timestamptz[]) \
         ON CONFLICT (course_id, session_date, start_time) DO NOTHING",
    )
    .bind(&ids)
    .bind(&c_ids)
    .bind(&dates)
    .bind(&starts)
    .bind(&ends)
    .bind(&venues)
    .bind(candidates.len() as i32)
    .execute(db)
    .await?;

    Ok(())
}

/// A new course's first weekly schedule (`courses::service::create_course`),
/// inside the caller's transaction so it commits atomically with the course
/// row. No reconcile step — a brand-new course has no sessions to align.
pub async fn set_initial_schedule_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    slots: &[SlotRow],
) -> Result<(), sqlx::Error> {
    replace_slots_tx(tx, course_id, slots).await
}

/// Replace a course's weekly schedule and align its future sessions to it
/// (`courses::service::update_course`, when the PATCH body carried
/// `schedule_slots`), inside the caller's transaction. `today` is the
/// studio-local calendar date; see [`reconcile_future_sessions_tx`] for what
/// "future" and "align" mean.
pub async fn replace_weekly_schedule_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    slots: &[SlotRow],
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    replace_slots_tx(tx, course_id, slots).await?;
    reconcile_future_sessions_tx(tx, course_id, today).await
}

/// Replace all of a course's weekly slots within an already-open
/// transaction (delete + insert). Each tuple is `(day_of_week, start_time,
/// end_time, venue)` — already parsed/validated by the caller.
async fn replace_slots_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    slots: &[SlotRow],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM course_schedule_slots WHERE course_id = $1")
        .bind(course_id)
        .execute(&mut **tx)
        .await?;

    for (day_of_week, start_time, end_time, venue) in slots {
        sqlx::query(
            "INSERT INTO course_schedule_slots \
             (id, course_id, day_of_week, start_time, end_time, venue, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NOW())",
        )
        .bind(Uuid::now_v7())
        .bind(course_id)
        .bind(day_of_week)
        .bind(start_time)
        .bind(end_time)
        .bind(venue.as_deref())
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

/// Reconcile a course's future `course_sessions` rows against its current
/// `course_schedule_slots` right after [`replace_slots_tx`] replaced them in
/// the same transaction. "Future" = strictly after `today` (the studio-local
/// calendar date); today's sessions are never touched, and no new session
/// is materialized here (the read side still does that lazily via
/// `materialize_range`).
///
/// Two steps, both keyed by [`SLOT_OF_SESSION`] — the same correspondence
/// `materialize_range` uses:
/// 1. UPDATE — a future session whose `(day_of_week, start_time)` still
///    matches a slot has its `end_time` and `venue` synced to that slot's
///    current values (a slot edit that only changes `end_time`/`venue` must
///    not orphan the session; today's and past sessions keep their venue
///    snapshot).
/// 2. DELETE — a future session with no matching slot at all is an orphan.
///    It's only deleted when nothing references it: no `leave_requests`
///    row (any status) via `session_id` *or* `makeup_session_id` — both
///    are `NO ACTION` FKs, so deleting a referenced row would 23503 — and
///    no `attendance_records` row (that FK is `ON DELETE CASCADE`, so this
///    guard is the only thing standing between an orphan session and
///    silently vanishing attendance history).
///
/// Accepted race: a leave/makeup request can be INSERTed against this same
/// session between step 2's `NOT EXISTS` check and its `DELETE` — the FK
/// then makes the DELETE fail with `23503` (surfaces as 500) instead of
/// silently orphaning the new leave row. Rare, no bad data results, and a
/// retry (which re-evaluates `NOT EXISTS` and now sees the row referenced)
/// succeeds.
///
/// Same class of race on the materialize side: `materialize_range`'s own
/// slot SELECT → INSERT window can straddle an `update_course` commit. A
/// future session materialized from the pre-commit slot snapshot then keeps
/// the old venue (like the old `end_time`) until the next course edit runs
/// this reconcile again.
async fn reconcile_future_sessions_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
    today: NaiveDate,
) -> Result<(), sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE course_sessions cs \
         SET end_time = s.end_time, venue = s.venue \
         FROM course_schedule_slots s \
         WHERE cs.course_id = $1 \
           AND cs.session_date > $2 \
           AND {SLOT_OF_SESSION} \
           AND (s.end_time <> cs.end_time OR s.venue IS DISTINCT FROM cs.venue)"
    )))
    .bind(course_id)
    .bind(today)
    .execute(&mut **tx)
    .await?;

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM course_sessions cs \
         WHERE cs.course_id = $1 \
           AND cs.session_date > $2 \
           AND NOT EXISTS ( \
             SELECT 1 FROM course_schedule_slots s \
             WHERE {SLOT_OF_SESSION} \
           ) \
           AND NOT EXISTS ( \
             SELECT 1 FROM leave_requests lr \
             WHERE lr.session_id = cs.id OR lr.makeup_session_id = cs.id \
           ) \
           AND NOT EXISTS ( \
             SELECT 1 FROM attendance_records ar WHERE ar.session_id = cs.id \
           )"
    )))
    .bind(course_id)
    .bind(today)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
