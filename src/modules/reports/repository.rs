use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::modules::orders::model::REVENUE_STATUSES;

use super::model::{ActivityRow, AdminCoachRow, AdminCourseRow, IncomeSourceRow, KpiRow};

/// `attendance_records.status` values shared by [`coach_attendance_in_range`],
/// [`member_attendance`] and [`kpis`] — one spelling of each literal instead
/// of three.
const PRESENT: &str = "present";
const ABSENT: &str = "absent";

/// 場租計收的 booking 狀態(Round 4 Phase 4 口徑):**場租計收 = status ∈
/// confirmed/completed 的 bookings 之 `price_cents` 快照,歸屬 slot 使用日
/// (非下訂日)**。`pending`/`cancelled`/`no_show` 一律不入 — the venue-rental
/// twin of `orders::model::REVENUE_STATUSES`, kept separate because
/// `booking_status` is its own state machine, not the order one.
const VENUE_REVENUE_STATUSES: [&str; 2] = ["confirmed", "completed"];

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

/// 12 monthly revenue buckets ending at `now`'s `tz_name`-local month
/// (oldest first). `orders.paid_at` is converted to `tz_name`'s wall-clock
/// time before truncating to a month, so bucketing follows the studio's
/// calendar rather than whatever timezone the DB session happens to be in.
/// Revenue counts only the "paid family" (`paid`/`processing`/`completed`)
/// — `refunded`/`cancelled`/`pending` orders never contribute (a refunded
/// order keeps its original `paid_at` per `orders::repository::
/// update_status_and_paid_at_tx`, so this is a status filter, not a
/// `paid_at IS NOT NULL` filter). The `generate_series` LEFT JOIN
/// guarantees exactly 12 rows even when `orders` is empty — no month can
/// "disappear" for lack of matching rows.
pub async fn revenue_trend(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT to_char(m.month_start, 'YYYY-MM') AS month, \
                COALESCE(SUM(o.total_cents), 0)::bigint AS revenue_cents \
         FROM generate_series( \
                date_trunc('month', $1::timestamptz AT TIME ZONE $2) - interval '11 months', \
                date_trunc('month', $1::timestamptz AT TIME ZONE $2), \
                interval '1 month' \
              ) AS m(month_start) \
         LEFT JOIN orders o \
           ON date_trunc('month', o.paid_at AT TIME ZONE $2) = m.month_start \
          AND o.status::text = ANY($3) \
         GROUP BY m.month_start \
         ORDER BY m.month_start",
    )
    .bind(now)
    .bind(tz_name)
    .bind(&REVENUE_STATUSES[..])
    .fetch_all(db)
    .await
}

/// `(total, new_this_month, active)` — one row, three scalar aggregates.
/// `total`/`new_this_month` count every `users` row (no role filter, per
/// task brief); `active` = distinct users holding at least one `active`
/// enrolment.
pub async fn member_stats(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<(i64, i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COUNT(*), \
                COUNT(*) FILTER ( \
                  WHERE date_trunc('month', u.created_at AT TIME ZONE $2) \
                      = date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
                ), \
                (SELECT COUNT(DISTINCT user_id) FROM enrolments WHERE status = 'active') \
         FROM users u",
    )
    .bind(now)
    .bind(tz_name)
    .fetch_one(db)
    .await
}

/// The admin report's KPI counts — one SELECT of scalar subqueries over a
/// single-row `anchor` CTE holding the current studio-local month start, so
/// "this month"/"last month" are computed once and every subquery agrees on
/// the boundary (`AT TIME ZONE` + `date_trunc`, same convention as
/// [`revenue_trend`]).
///
/// Pairs (this/last studio month):
/// - `new_members` — `users.created_at` (no role filter, matching
///   [`member_stats`]'s `new_this_month`);
/// - `new_enrolments` — `enrolments.created_at`, `cancelled` excluded;
/// - `paid_orders` — `orders` in `REVENUE_STATUSES`, bucketed by `paid_at`
///   (the month the money moved — same anchor as [`revenue_trend`];
///   排除 pending/refunded 於一切金額聚合);
/// - `present`/`absent` — `attendance_records` bucketed by the session's
///   `session_date` (already a studio-local date per contract §3.18), for
///   the service's `attendance_rate` = present/(present+absent),`leave`
///   不入分母;無資料月 → null (via `service::safe_ratio`).
pub async fn kpis(db: &PgPool, now: DateTime<Utc>, tz_name: &str) -> Result<KpiRow, sqlx::Error> {
    sqlx::query_as::<_, KpiRow>(
        "WITH anchor AS ( \
           SELECT date_trunc('month', $1::timestamptz AT TIME ZONE $2) AS this_m \
         ) \
         SELECT \
           (SELECT COUNT(*) FROM users u \
             WHERE date_trunc('month', u.created_at AT TIME ZONE $2) = a.this_m) \
             AS new_members_this, \
           (SELECT COUNT(*) FROM users u \
             WHERE date_trunc('month', u.created_at AT TIME ZONE $2) \
                 = a.this_m - interval '1 month') AS new_members_last, \
           (SELECT COUNT(*) FROM enrolments e \
             WHERE e.status <> 'cancelled' \
               AND date_trunc('month', e.created_at AT TIME ZONE $2) = a.this_m) \
             AS new_enrolments_this, \
           (SELECT COUNT(*) FROM enrolments e \
             WHERE e.status <> 'cancelled' \
               AND date_trunc('month', e.created_at AT TIME ZONE $2) \
                 = a.this_m - interval '1 month') AS new_enrolments_last, \
           (SELECT COUNT(*) FROM orders o \
             WHERE o.status::text = ANY($3) \
               AND date_trunc('month', o.paid_at AT TIME ZONE $2) = a.this_m) \
             AS paid_orders_this, \
           (SELECT COUNT(*) FROM orders o \
             WHERE o.status::text = ANY($3) \
               AND date_trunc('month', o.paid_at AT TIME ZONE $2) \
                 = a.this_m - interval '1 month') AS paid_orders_last, \
           (SELECT COUNT(*) FROM attendance_records ar \
             JOIN course_sessions cs ON cs.id = ar.session_id \
             WHERE ar.status = $4::attendance_status \
               AND date_trunc('month', cs.session_date::timestamp) = a.this_m) \
             AS present_this, \
           (SELECT COUNT(*) FROM attendance_records ar \
             JOIN course_sessions cs ON cs.id = ar.session_id \
             WHERE ar.status = $5::attendance_status \
               AND date_trunc('month', cs.session_date::timestamp) = a.this_m) \
             AS absent_this, \
           (SELECT COUNT(*) FROM attendance_records ar \
             JOIN course_sessions cs ON cs.id = ar.session_id \
             WHERE ar.status = $4::attendance_status \
               AND date_trunc('month', cs.session_date::timestamp) \
                 = a.this_m - interval '1 month') AS present_last, \
           (SELECT COUNT(*) FROM attendance_records ar \
             JOIN course_sessions cs ON cs.id = ar.session_id \
             WHERE ar.status = $5::attendance_status \
               AND date_trunc('month', cs.session_date::timestamp) \
                 = a.this_m - interval '1 month') AS absent_last \
         FROM anchor a",
    )
    .bind(now)
    .bind(tz_name)
    .bind(&REVENUE_STATUSES[..])
    .bind(PRESENT)
    .bind(ABSENT)
    .fetch_one(db)
    .await
}

/// Per-(month, source) gross income for the trailing `months` studio-local
/// months (oldest first) — the shared aggregation behind both
/// `revenue_breakdown` (current-month rows) and `income_sources_12m`.
///
/// 口徑 (Round 4 Phase 4):
/// - **breakdown/income line 金額 = 折扣前毛額**(`order_items` 的 line 小計
///   `unit_price_cents * quantity`),order 層 `discount` **不攤分**;「實收」
///   由既有 revenue section(`orders.total_cents`)表達,兩者口徑差異在此。
/// - source 值域:`course`(item_type = course 的 line)/ `ticket` /
///   `membership` / `course_package` / `merchandise`(product line 按
///   `products.product_type`)+ **venue_rental**(bookings 快照價,見
///   [`VENUE_REVENUE_STATUSES`],歸屬 **slot 使用日**(非下訂日)——
///   `time_slots.date` is already a studio-local date per contract §3.18,
///   so it needs no `AT TIME ZONE` shift)。
/// - order lines 歸屬 `paid_at` 的 studio 月份;**排除 pending/refunded 於
///   一切金額聚合**(status ∈ `REVENUE_STATUSES` — a refunded order keeps
///   its `paid_at`, so this must be a status filter)。
///
/// The `generate_series` × fixed-`VALUES` source list CROSS JOIN zero-fills
/// every (month, source) cell — like [`revenue_trend`], no bucket can
/// "disappear" for lack of rows, so consumers always see `months × 6` rows
/// in (month asc, canonical source order). `orders_count` counts distinct
/// orders (bookings for the venue arm); `units` sums line quantities (one
/// booking = 1). The belt-and-suspenders `paid_at IS NOT NULL` mirrors
/// [`recent_activity`].
pub async fn income_by_source(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
    months: i32,
) -> Result<Vec<IncomeSourceRow>, sqlx::Error> {
    sqlx::query_as::<_, IncomeSourceRow>(
        "WITH months AS ( \
           SELECT generate_series( \
                    date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
                      - ($3 - 1) * interval '1 month', \
                    date_trunc('month', $1::timestamptz AT TIME ZONE $2), \
                    interval '1 month' \
                  ) AS month_start \
         ), \
         sources(source, ord) AS ( \
           VALUES ('course', 1), ('ticket', 2), ('membership', 3), \
                  ('course_package', 4), ('merchandise', 5), ('venue_rental', 6) \
         ), \
         line_income AS ( \
           SELECT date_trunc('month', o.paid_at AT TIME ZONE $2) AS month_start, \
                  CASE WHEN oi.item_type = 'course' THEN 'course' \
                       ELSE p.product_type::text END AS source, \
                  (oi.unit_price_cents * oi.quantity)::bigint AS gross_cents, \
                  o.id AS revenue_event_id, \
                  oi.quantity::bigint AS units \
             FROM order_items oi \
             JOIN orders o ON o.id = oi.order_id \
             LEFT JOIN products p ON p.id = oi.product_id \
            WHERE o.status::text = ANY($4) AND o.paid_at IS NOT NULL \
           UNION ALL \
           SELECT date_trunc('month', ts.date::timestamp) AS month_start, \
                  'venue_rental' AS source, \
                  b.price_cents AS gross_cents, \
                  b.id AS revenue_event_id, \
                  1::bigint AS units \
             FROM bookings b \
             JOIN time_slots ts ON ts.id = b.time_slot_id \
            WHERE b.status::text = ANY($5) \
         ) \
         SELECT to_char(m.month_start, 'YYYY-MM') AS month, \
                s.source, \
                COALESCE(SUM(li.gross_cents), 0)::bigint AS gross_cents, \
                COUNT(DISTINCT li.revenue_event_id)::bigint AS orders_count, \
                COALESCE(SUM(li.units), 0)::bigint AS units \
           FROM months m \
          CROSS JOIN sources s \
           LEFT JOIN line_income li \
             ON li.month_start = m.month_start AND li.source = s.source \
          GROUP BY m.month_start, s.source, s.ord \
          ORDER BY m.month_start, s.ord",
    )
    .bind(now)
    .bind(tz_name)
    .bind(months)
    .bind(&REVENUE_STATUSES[..])
    .bind(&VENUE_REVENUE_STATUSES[..])
    .fetch_all(db)
    .await
}

/// `(payment_method, order count)` for the current studio month — 口徑
/// (Round 4 Phase 4): **本月(studio 時區)`REVENUE_STATUSES` 訂單筆數占比;
/// `payment_method` NULL → `\"unknown\"` 鍵原樣輸出(前端顯示「其他」)**。
/// Counts whole orders (not lines, not amounts), bucketed by `paid_at` like
/// every other money aggregate; the ratio itself is the frontend's division
/// (環比 delta 同樣前端算). Methods with zero orders simply don't appear —
/// the value domain is application-level (`orders::model::PAYMENT_METHODS`),
/// not a DB enum, so there is no fixed list to zero-fill against.
pub async fn payment_split(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT COALESCE(o.payment_method, 'unknown') AS method, COUNT(*)::bigint AS orders \
         FROM orders o \
         WHERE o.status::text = ANY($3) \
           AND date_trunc('month', o.paid_at AT TIME ZONE $2) \
             = date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
         GROUP BY 1 \
         ORDER BY orders DESC, method",
    )
    .bind(now)
    .bind(tz_name)
    .bind(&REVENUE_STATUSES[..])
    .fetch_all(db)
    .await
}

/// Every course with its live `enrolled`/`waitlist_count` (correlated
/// subqueries — same pattern as `courses::model::Course.enrolled_count`),
/// so `fill_rate` can be derived in `service` without a second query.
pub async fn course_reports(db: &PgPool) -> Result<Vec<AdminCourseRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminCourseRow>(
        "SELECT c.id AS course_id, c.name, \
                (SELECT COUNT(*) FROM enrolments e \
                  WHERE e.course_id = c.id AND e.status = 'active') AS enrolled, \
                c.max_students, \
                (SELECT COUNT(*) FROM waitlist_entries w \
                  WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count \
         FROM courses c \
         ORDER BY c.name, c.id",
    )
    .fetch_all(db)
    .await
}

/// Every coach with their `course_count`/`student_count`/
/// `revenue_cents_12m` (correlated subqueries, one query, no N+1).
///
/// `revenue_cents_12m` 口徑 (Round 4 Phase 4): **coach 營收歸因 = course 類
/// order line 毛額歸 `courses.coach_id`;票券/裝備/場租不歸因**。Gross =
/// 折扣前 line 小計 (`unit_price_cents * quantity`, order 層 discount
/// 不攤分), orders in `REVENUE_STATUSES` only (排除 pending/refunded),
/// attributed to `paid_at`'s studio month across the same trailing
/// 12-month window as [`revenue_trend`] (current month + 11 before it).
/// The join on `oi.course_id` *is* the course-line filter — the
/// `order_items_one_target` CHECK guarantees only `item_type = 'course'`
/// lines have a non-NULL `course_id`.
pub async fn coach_reports(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<Vec<AdminCoachRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminCoachRow>(
        "SELECT co.id AS coach_id, u.name, \
                (SELECT COUNT(*) FROM courses c \
                  WHERE c.coach_id = co.id) AS course_count, \
                (SELECT COUNT(DISTINCT e.user_id) FROM enrolments e \
                   JOIN courses c ON c.id = e.course_id \
                  WHERE c.coach_id = co.id AND e.status = 'active') AS student_count, \
                (SELECT COALESCE(SUM(oi.unit_price_cents * oi.quantity), 0) \
                   FROM order_items oi \
                   JOIN orders o ON o.id = oi.order_id \
                   JOIN courses c ON c.id = oi.course_id \
                  WHERE c.coach_id = co.id \
                    AND o.status::text = ANY($3) \
                    AND o.paid_at IS NOT NULL \
                    AND date_trunc('month', o.paid_at AT TIME ZONE $2) \
                        BETWEEN date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
                                  - interval '11 months' \
                            AND date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
                )::bigint AS revenue_cents_12m \
         FROM coaches co \
         JOIN users u ON u.id = co.user_id \
         ORDER BY u.name, co.id",
    )
    .bind(now)
    .bind(tz_name)
    .bind(&REVENUE_STATUSES[..])
    .fetch_all(db)
    .await
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

/// `(today_sessions, pending_attendance)` for `coach_id`'s courses on
/// `today`. Caller must materialize `today` first (see
/// `sessions::repository::materialize_range`) — this only counts
/// already-existing `course_sessions` rows.
pub async fn coach_today_and_pending(
    db: &PgPool,
    coach_id: Uuid,
    today: NaiveDate,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), \
                COUNT(*) FILTER (WHERE NOT EXISTS ( \
                  SELECT 1 FROM attendance_records ar WHERE ar.session_id = cs.id \
                )) \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE c.coach_id = $1 AND cs.session_date = $2",
    )
    .bind(coach_id)
    .bind(today)
    .fetch_one(db)
    .await
}

/// Total unread messages across every conversation `user_id` participates
/// in, on either side — mirrors `messages::repository::
/// find_my_conversations`'s per-conversation `unread_count` correlated
/// subquery, aggregated here to one grand total instead of one row per
/// conversation.
pub async fn unread_message_count(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM messages m \
         JOIN conversations c ON c.id = m.conversation_id \
         WHERE (c.member_id = $1 OR c.coach_id = $1) \
           AND m.sender_id <> $1 AND m.read_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Distinct students across `coach_id`'s *active* courses' *active*
/// enrolments. Mirrors `attendance::repository::find_my_students`'s WHERE
/// clause (copied, not shared — that module's own comment documents this
/// as the established convention) but as a bare count, without the
/// `jsonb_agg` course list `find_my_students` builds for its own response.
pub async fn coach_student_count(db: &PgPool, coach_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT e.user_id) FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE c.coach_id = $1 AND c.is_active = true AND e.status = 'active'",
    )
    .bind(coach_id)
    .fetch_one(db)
    .await
}

/// `(present_count, absent_count)` across `coach_id`'s courses' sessions in
/// `[from, to]`. `leave` rows are never selected into either bucket — the
/// brief's "leave 不入分母" rule.
pub async fn coach_attendance_in_range(
    db: &PgPool,
    coach_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*) FILTER (WHERE ar.status = $4::attendance_status), \
                COUNT(*) FILTER (WHERE ar.status = $5::attendance_status) \
         FROM attendance_records ar \
         JOIN course_sessions cs ON cs.id = ar.session_id \
         JOIN courses c ON c.id = cs.course_id \
         WHERE c.coach_id = $1 AND cs.session_date BETWEEN $2 AND $3",
    )
    .bind(coach_id)
    .bind(from)
    .bind(to)
    .bind(PRESENT)
    .bind(ABSENT)
    .fetch_one(db)
    .await
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

/// `(present_count, absent_count)` across every session ever marked for any
/// of `user_id`'s enrolments — not filtered by enrolment status, since a
/// cancelled enrolment doesn't erase attendance history that already
/// happened. `leave` rows are never selected into either bucket.
pub async fn member_attendance(db: &PgPool, user_id: Uuid) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*) FILTER (WHERE ar.status = $2::attendance_status), \
                COUNT(*) FILTER (WHERE ar.status = $3::attendance_status) \
         FROM attendance_records ar \
         JOIN enrolments e ON e.id = ar.enrolment_id \
         WHERE e.user_id = $1",
    )
    .bind(user_id)
    .bind(PRESENT)
    .bind(ABSENT)
    .fetch_one(db)
    .await
}

pub async fn points_balance(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT points_balance FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Course ids of `user_id`'s *active* enrolments. `uniq_enrolments_active`
/// guarantees at most one active enrolment per (user, course), so the
/// returned row count already equals `active_enrolments` — the caller
/// doesn't need a separate COUNT query.
pub async fn my_active_enrolment_course_ids(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT course_id FROM enrolments WHERE user_id = $1 AND status = 'active'",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Count of `course_ids`' materialized sessions in `[from, to]`. Caller
/// must materialize that range first (see
/// `sessions::repository::materialize_range`).
pub async fn upcoming_session_count(
    db: &PgPool,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<i64, sqlx::Error> {
    if course_ids.is_empty() {
        return Ok(0);
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM course_sessions \
         WHERE course_id = ANY($1::uuid[]) AND session_date BETWEEN $2 AND $3",
    )
    .bind(course_ids)
    .bind(from)
    .bind(to)
    .fetch_one(db)
    .await
}

// ---------------------------------------------------------------------------
// GET /reports/admin/activity
// ---------------------------------------------------------------------------

/// Recent operational events across four sources — new user signups, paid
/// orders, new enrolments, and new contact inquiries — merged and sorted by
/// `occurred_at` descending, capped at 20. Each branch is independently
/// `ORDER BY ... DESC LIMIT 20` *before* the outer UNION ALL sorts and caps
/// again to 20 total, so no branch ever needs a full table scan regardless
/// of how large `users`/`orders`/`enrolments`/`contact_inquiries` grow — and
/// since the final result only ever has 20 slots, no branch could
/// contribute more than 20 rows to it anyway, so per-branch LIMIT 20 never
/// drops a row that should have made the final cut.
///
/// Orders count as "paid" via `status::text = ANY($1)` against
/// `REVENUE_STATUSES` (the same status set `revenue_trend` uses for the
/// admin report's revenue section) rather than `paid_at IS NOT NULL` alone
/// — a refunded/cancelled order keeps its original `paid_at` (see
/// `orders::repository::update_status_and_paid_at_tx`) but has left the
/// paid family, and surfacing it as a fresh "已付款" event would misrepresent
/// its current state. The extra `paid_at IS NOT NULL` guard is
/// belt-and-suspenders: every row reaching `status = ANY($1)` is guaranteed
/// one by the order status machine, but this keeps decoding into a non-
/// `Option<DateTime<Utc>>` `occurred_at` column provably safe at the SQL
/// level too, not just by app-level invariant.
///
/// The contact-inquiry branch's `detail` falls back to `name` when
/// `subject` is empty (`COALESCE(NULLIF(subject, ''), name)`) per the task
/// brief's "{subject 或 name}" wording, even though `subject` is currently
/// `NOT NULL` and application-validated non-empty on every write path — a
/// defensive fallback for stored data the validation layer didn't produce.
pub async fn recent_activity(db: &PgPool) -> Result<Vec<ActivityRow>, sqlx::Error> {
    sqlx::query_as::<_, ActivityRow>(
        "SELECT * FROM ( \
           (SELECT 'user' AS kind, name AS detail, NULL::bigint AS amount_cents, \
                   NULL::text AS inquiry_type, created_at AS occurred_at \
            FROM users \
            ORDER BY created_at DESC LIMIT 20) \
           UNION ALL \
           (SELECT 'order' AS kind, order_number AS detail, total_cents AS amount_cents, \
                   NULL::text AS inquiry_type, paid_at AS occurred_at \
            FROM orders \
            WHERE status::text = ANY($1) AND paid_at IS NOT NULL \
            ORDER BY paid_at DESC LIMIT 20) \
           UNION ALL \
           (SELECT 'enrolment' AS kind, c.name AS detail, NULL::bigint AS amount_cents, \
                   NULL::text AS inquiry_type, e.created_at AS occurred_at \
            FROM enrolments e \
            JOIN courses c ON c.id = e.course_id \
            ORDER BY e.created_at DESC LIMIT 20) \
           UNION ALL \
           (SELECT 'inquiry' AS kind, COALESCE(NULLIF(subject, ''), name) AS detail, \
                   NULL::bigint AS amount_cents, inquiry_type::text AS inquiry_type, \
                   created_at AS occurred_at \
            FROM contact_inquiries \
            ORDER BY created_at DESC LIMIT 20) \
         ) merged \
         ORDER BY occurred_at DESC \
         LIMIT 20",
    )
    .bind(&REVENUE_STATUSES[..])
    .fetch_all(db)
    .await
}
