use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

/// One entry of `revenue.trend` — oldest first, 12 entries, zero-filled for
/// months with no paid-family revenue (see `repository::revenue_trend`).
#[derive(Debug, Serialize)]
pub struct RevenueMonthPoint {
    pub month: String,
    pub revenue_cents: i64,
}

#[derive(Debug, Serialize)]
pub struct AdminRevenueSection {
    pub this_month_cents: i64,
    pub last_month_cents: i64,
    pub trend: Vec<RevenueMonthPoint>,
}

#[derive(Debug, Serialize)]
pub struct AdminMembersSection {
    pub total: i64,
    pub new_this_month: i64,
    pub active: i64,
}

/// `fill_rate` is `None` when `max_students` is 0 — cannot happen through
/// normal writes (`courses_max_students_pos CHECK (max_students > 0)`), but
/// the divide-by-zero guard is defensive rather than trusting the DB
/// constraint to hold forever (see `service::safe_ratio`).
#[derive(Debug, Serialize)]
pub struct AdminCourseReportRow {
    pub course_id: Uuid,
    pub name: String,
    pub enrolled: i64,
    pub max_students: i32,
    pub fill_rate: Option<f64>,
    pub waitlist_count: i64,
}

/// `revenue_cents_12m` (Round 4 Phase 4) 口徑:**coach 營收歸因 = course 類
/// order line 毛額歸 `courses.coach_id`;票券/裝備/場租不歸因**。折扣前
/// line 小計,orders ∈ `REVENUE_STATUSES`,`paid_at` 落在近 12 個 studio
/// 月(與 `revenue.trend` 同窗)。
///
/// `attendance_rate` (Round 4 Phase 4) 口徑:**該教練課程的 present/(present
/// +absent),`leave` 不入分母**(全期,無出勤資料 → null,與 `service::
/// safe_ratio` 同語意)。
#[derive(Debug, Serialize)]
pub struct AdminCoachReportRow {
    pub coach_id: Uuid,
    pub name: String,
    pub course_count: i64,
    pub student_count: i64,
    pub revenue_cents_12m: i64,
    pub attendance_rate: Option<f64>,
}

/// One fixed-bucket cell of the three human-flow distributions
/// (`attendance_distribution` / `age_distribution` / `tier_distribution`) —
/// backend-neutral `bucket` key + member `count`. Shared 口徑:
/// - **attendance_distribution**:每會員 present/(present+absent)(`leave`
///   不入分母;**未點名/僅請假(分母為 0)的會員不入分布**)分入固定 4 桶
///   `gte_95`(95–100%)/ `85_94` / `75_84` / `lt_75`(低於 75%)。
/// - **age_distribution**:`birth_date` 算年齡分入固定 6 桶 `0-6` / `7-12` /
///   `13-17` / `18-25` / `26-40` / `41+`,**排除 `birth_date` NULL**。
/// - **tier_distribution**:`points_balance` 分入固定 4 桶 `regular`(<500)/
///   `bronze`(500–1999)/ `silver`(2000–4999)/ `gold`(≥5000)。
///
/// 每種分布一律零填其固定桶(空桶 `count = 0`),中文標籤前端配。
#[derive(Debug, Serialize)]
pub struct BucketCountEntry {
    pub bucket: String,
    pub count: i64,
}

/// One of `retention`'s trailing-6-month cohort rows (oldest first, 6 桶
/// 零填). 口徑:會員在 M 月有 ≥1 筆 `present` 即「M 月活躍」;首次活躍月
/// 計入 `new_count`、此後再活躍計入 `returning_count`;`rate` =
/// |上月活躍 ∩ 本月活躍| / |上月活躍|,**上月為空集合 → null**(undefined,
/// 非 0)。`month` 為 studio 時區的 `YYYY-MM`。
#[derive(Debug, Serialize)]
pub struct RetentionMonthRow {
    pub month: String,
    pub new_count: i64,
    pub returning_count: i64,
    pub rate: Option<f64>,
}

/// One weekday bucket of `weekday_load` — `weekday` `0=週日..6=週六`(§3.18
/// 慣例),`present_count` 為近 30 天已物化場次的 `present` 出席人次(7 桶
/// 零填)。
#[derive(Debug, Serialize)]
pub struct WeekdayLoadEntry {
    pub weekday: i16,
    pub present_count: i64,
}

/// One venue row of `venue_usage` — 本月(studio 時區)該 `venue` 名下場次
/// 的分鐘數合計(`venue` 為 NULL 的場次不入;非固定桶,無場次的場地不出列)。
#[derive(Debug, Serialize)]
pub struct VenueUsageEntry {
    pub venue: String,
    pub minutes: i64,
}

/// `funnel` — 誠實 2 段(近 90 studio 天):`trial_inquiries`
/// (`inquiry_type='trial'` 洽詢計數)→ `new_enrolments`(`enrolments`
/// created,不含 `cancelled`)。不造中間段。
#[derive(Debug, Serialize)]
pub struct FunnelSection {
    pub trial_inquiries: i64,
    pub new_enrolments: i64,
}

/// A this/last studio-month count pair. 環比 delta 不算(前端算)— the
/// backend only ships the two raw counts.
#[derive(Debug, Serialize)]
pub struct MonthPair {
    pub this_month: i64,
    pub last_month: i64,
}

/// A this/last studio-month ratio pair (0–1). `None` = 無資料月 → null —
/// undefined, not zero (see `service::safe_ratio`).
#[derive(Debug, Serialize)]
pub struct RateMonthPair {
    pub this_month: Option<f64>,
    pub last_month: Option<f64>,
}

/// `GET /reports/admin`'s `kpis` section (Round 4 Phase 4) — three
/// this/last studio-month count pairs plus the attendance-rate pair:
/// - `new_members`: `users` created(無 role 過濾,與 `members.
///   new_this_month` 同口徑);
/// - `new_enrolments`: `enrolments` created,不含 `cancelled`;
/// - `paid_orders_count`: `REVENUE_STATUSES` 訂單(排除 pending/refunded),
///   歸屬 `paid_at` 的 studio 月份;
/// - `attendance_rate`: present/(present+absent),`leave` 不入分母;
///   無資料月 → null。
#[derive(Debug, Serialize)]
pub struct KpisSection {
    pub new_members: MonthPair,
    pub new_enrolments: MonthPair,
    pub paid_orders_count: MonthPair,
    pub attendance_rate: RateMonthPair,
}

/// One source's row of `revenue_breakdown` (current studio month, all 6
/// sources always present in canonical order: `course`/`ticket`/
/// `membership`/`course_package`/`merchandise`/`venue_rental`).
///
/// 口徑:**breakdown/income line 金額 = 折扣前毛額**(`order_items` 的 line
/// 小計),order 層 discount **不攤分**;「實收」由既有 `revenue` section
/// 表達,兩者口徑差異在此。**場租計收 = status ∈ confirmed/completed 的
/// bookings 之 `price_cents` 快照,歸屬 slot 使用日(非下訂日)**;order
/// lines 歸屬 `paid_at` 月份,**排除 pending/refunded 於一切金額聚合**。
/// `orders_count` = 觸及該 source 的訂單數(場租為 booking 數);`units` =
/// line quantity 合計(一筆 booking = 1)。
#[derive(Debug, Serialize)]
pub struct IncomeSourceEntry {
    pub source: String,
    pub gross_cents: i64,
    pub orders_count: i64,
    pub units: i64,
}

/// One (month, source) row of `income_sources_12m` — the trailing 12
/// studio months × 6 sources (72 rows, zero-filled, oldest month first,
/// same window as `revenue.trend`). Same 口徑 as [`IncomeSourceEntry`],
/// with `month` as `YYYY-MM`.
#[derive(Debug, Serialize)]
pub struct IncomeSourceMonthEntry {
    pub month: String,
    pub source: String,
    pub gross_cents: i64,
    pub orders_count: i64,
    pub units: i64,
}

/// One source's row of `category_split` — 本月 **order line 毛額**按 source
/// 值域占比(`course`/`ticket`/`membership`/`course_package`/`merchandise`,
/// 與 `revenue_breakdown` 同口徑、同一次聚合派生;場租非 order line,不在
/// 此 split)。`ratio` ∈ 0–1,分母為本月五桶毛額合計;合計為 0 時 → null
/// (undefined, not 0)。
#[derive(Debug, Serialize)]
pub struct CategorySplitEntry {
    pub source: String,
    pub gross_cents: i64,
    pub ratio: Option<f64>,
}

/// One row of `payment_split` — 口徑:**本月(studio 時區)`REVENUE_STATUSES`
/// 訂單筆數占比;`payment_method` NULL → `"unknown"` 鍵原樣輸出(前端顯示
/// 「其他」)**。後端只出 `{ method, count }`(筆數),占比與環比 delta
/// 前端算;零筆的 method 不出列。
#[derive(Debug, Serialize)]
pub struct PaymentSplitEntry {
    pub method: String,
    pub count: i64,
}

/// `GET /reports/admin` — 既有 `revenue`/`members`/`courses`/`coaches` 加
/// Round 4 Phase 4 金流 sections(`kpis`/`revenue_breakdown`/
/// `income_sources_12m`/`category_split`/`payment_split`)與人流 sections
/// (`attendance_distribution`/`age_distribution`/`tier_distribution`/
/// `retention`/`funnel`/`weekday_load`/`venue_usage`)。空庫時所有 section
/// 零填/空陣列,絕不 500。
#[derive(Debug, Serialize)]
pub struct AdminReportResponse {
    pub revenue: AdminRevenueSection,
    pub kpis: KpisSection,
    pub revenue_breakdown: Vec<IncomeSourceEntry>,
    pub income_sources_12m: Vec<IncomeSourceMonthEntry>,
    pub category_split: Vec<CategorySplitEntry>,
    pub payment_split: Vec<PaymentSplitEntry>,
    pub attendance_distribution: Vec<BucketCountEntry>,
    pub age_distribution: Vec<BucketCountEntry>,
    pub tier_distribution: Vec<BucketCountEntry>,
    pub retention: Vec<RetentionMonthRow>,
    pub funnel: FunnelSection,
    pub weekday_load: Vec<WeekdayLoadEntry>,
    pub venue_usage: Vec<VenueUsageEntry>,
    pub members: AdminMembersSection,
    pub courses: Vec<AdminCourseReportRow>,
    pub coaches: Vec<AdminCoachReportRow>,
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CoachReportResponse {
    pub today_sessions: i64,
    pub pending_attendance: i64,
    pub unread_messages: i64,
    pub student_count: i64,
    pub attendance_rate_30d: Option<f64>,
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MemberReportResponse {
    pub attended_total: i64,
    pub attendance_rate: Option<f64>,
    pub points_balance: i64,
    pub active_enrolments: i64,
    pub upcoming_sessions_7d: i64,
}

// ---------------------------------------------------------------------------
// GET /reports/admin/activity
// ---------------------------------------------------------------------------

/// One entry of `GET /reports/admin/activity`'s `items` — `label` is a
/// backend-composed, Traditional Chinese human-readable string (see
/// `service::activity_label`); `kind` lets the frontend pick an icon.
#[derive(Debug, Serialize)]
pub struct ActivityItem {
    pub kind: String,
    pub label: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ActivityResponse {
    pub items: Vec<ActivityItem>,
}
