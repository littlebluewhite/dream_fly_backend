//! `AdminReportInputs` → `AdminReportResponse` — the pure core (純核) for
//! `GET /reports/admin`'s row-shaping/derivation half. `service::admin_report`
//! runs the 13 sequential repository queries (plus the `venue_usage`
//! materialization witness dance) and then calls [`assemble_admin_report`]
//! once with everything it fetched; every ratio/position-index/filter/split
//! rule that used to sit inline in `service::admin_report` after its last
//! `.await` now lives here instead — zero IO, owned output, in-crate
//! `#[cfg(test)]` — same discipline as `orders::fulfilment`/
//! `subscriptions::entitlement`.
//!
//! `current_month_key` is a separate parameter rather than an
//! [`AdminReportInputs`] field — "server/now 不進純核": it is derived from
//! `studio_clock::month_key`, itself derived from the caller's sampled
//! `now`, and keeping it out of the struct keeps every field in
//! `AdminReportInputs` a plain repository-query result, nothing
//! server/clock-flavored mixed in.

use super::dto::{
    AdminCoachReportRow, AdminCourseReportRow, AdminMembersSection, AdminReportResponse,
    AdminRevenueSection, BucketCountEntry, CategorySplitEntry, FunnelSection, IncomeSourceEntry,
    IncomeSourceMonthEntry, KpisSection, MonthPair, PaymentSplitEntry, RateMonthPair,
    RetentionMonthRow, RevenueMonthPoint, VenueUsageEntry, WeekdayLoadEntry,
};
use super::model::{
    AdminCoachRow, AdminCourseRow, BucketCountRow, FunnelRow, IncomeSourceRow, KpiRow,
    RetentionRow, VenueUsageRow, WeekdayLoadRow,
};

/// The one source of `repository::income_by_source` that is *not* an order
/// line (bookings, not `order_items`) — `category_split` is defined over
/// order-line 毛額 only, so this source is filtered out of it (and out of
/// its ratio denominator) while still appearing in `revenue_breakdown`.
const VENUE_RENTAL_SOURCE: &str = "venue_rental";

/// Every repository result `service::admin_report`'s sequential queries
/// collect before assembly. Named fields are the only guard against the
/// three same-shaped `Vec<BucketCountRow>` fields (`attendance_dist_rows`/
/// `age_dist_rows`/`tier_dist_rows`) silently swapping at a call site.
pub struct AdminReportInputs {
    pub trend_rows: Vec<(String, i64)>,
    pub member_stats: (i64, i64, i64),
    pub course_rows: Vec<AdminCourseRow>,
    pub coach_rows: Vec<AdminCoachRow>,
    pub kpi: KpiRow,
    pub income_rows: Vec<IncomeSourceRow>,
    pub payment_rows: Vec<(String, i64)>,
    pub attendance_dist_rows: Vec<BucketCountRow>,
    pub age_dist_rows: Vec<BucketCountRow>,
    pub tier_dist_rows: Vec<BucketCountRow>,
    pub retention_rows: Vec<RetentionRow>,
    pub funnel_row: FunnelRow,
    pub weekday_rows: Vec<WeekdayLoadRow>,
    pub venue_rows: Vec<VenueUsageRow>,
}

/// `numerator / denominator` as a ratio, or `None` when `denominator` is 0.
/// Guards against emitting `NaN`/`Infinity` (which `serde_json` cannot
/// represent as valid JSON) and, more importantly, expresses "undefined"
/// explicitly rather than relying on that library-specific NaN/Infinity
/// serialization behavior. Shared by `fill_rate` (enrolled/max_students),
/// every attendance-rate calculation (present/(present+absent)), and
/// `category_split`'s gross-over-total ratios — all are "count over count,
/// zero-safe" in the same shape.
pub fn safe_ratio(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator as f64 / denominator as f64)
    }
}

/// One fixed-bucket distribution's rows (`attendance_dist_rows`/
/// `age_dist_rows`/`tier_dist_rows`) → its DTO entries — the shared
/// `(bucket, count)` rename that used to be repeated verbatim 3x in
/// `service::admin_report`.
fn bucket_entries(rows: Vec<BucketCountRow>) -> Vec<BucketCountEntry> {
    rows.into_iter().map(|r| BucketCountEntry { bucket: r.bucket, count: r.count }).collect()
}

/// Turn every repository result for `GET /reports/admin` into its response
/// shape — pure, zero IO. See the module doc for why `current_month_key` is
/// a parameter rather than an `AdminReportInputs` field.
pub fn assemble_admin_report(
    inputs: AdminReportInputs,
    current_month_key: &str,
) -> AdminReportResponse {
    let AdminReportInputs {
        trend_rows,
        member_stats: (total, new_this_month, active),
        course_rows,
        coach_rows,
        kpi,
        income_rows,
        payment_rows,
        attendance_dist_rows,
        age_dist_rows,
        tier_dist_rows,
        retention_rows,
        funnel_row,
        weekday_rows,
        venue_rows,
    } = inputs;

    let trend: Vec<RevenueMonthPoint> = trend_rows
        .into_iter()
        .map(|(month, revenue_cents)| RevenueMonthPoint { month, revenue_cents })
        .collect();
    // `revenue_trend` always returns exactly 12 rows (oldest..newest,
    // ending at the current studio-local month), so the last entry is
    // "this month" and the second-to-last is "last month". `checked_sub`
    // guards against a hypothetically shorter vec rather than assuming the
    // invariant always holds.
    let this_month_cents = trend.last().map(|p| p.revenue_cents).unwrap_or(0);
    let last_month_cents = trend
        .len()
        .checked_sub(2)
        .and_then(|i| trend.get(i))
        .map(|p| p.revenue_cents)
        .unwrap_or(0);

    let courses: Vec<AdminCourseReportRow> = course_rows
        .into_iter()
        .map(|r| AdminCourseReportRow {
            fill_rate: safe_ratio(r.enrolled, r.max_students as i64),
            course_id: r.course_id,
            name: r.name,
            enrolled: r.enrolled,
            max_students: r.max_students,
            waitlist_count: r.waitlist_count,
        })
        .collect();

    let coaches: Vec<AdminCoachReportRow> = coach_rows
        .into_iter()
        .map(|r| AdminCoachReportRow {
            coach_id: r.coach_id,
            name: r.name,
            course_count: r.course_count,
            student_count: r.student_count,
            revenue_cents_12m: r.revenue_cents_12m,
            attendance_rate: safe_ratio(r.att_present, r.att_present + r.att_absent),
        })
        .collect();

    let kpis = KpisSection {
        new_members: MonthPair {
            this_month: kpi.new_members_this,
            last_month: kpi.new_members_last,
        },
        new_enrolments: MonthPair {
            this_month: kpi.new_enrolments_this,
            last_month: kpi.new_enrolments_last,
        },
        paid_orders_count: MonthPair {
            this_month: kpi.paid_orders_this,
            last_month: kpi.paid_orders_last,
        },
        attendance_rate: RateMonthPair {
            this_month: safe_ratio(kpi.present_this, kpi.present_this + kpi.absent_this),
            last_month: safe_ratio(kpi.present_last, kpi.present_last + kpi.absent_last),
        },
    };

    // `income_by_source` zero-fills every (month, source) cell, so the
    // current studio month's rows are always exactly the 6 sources —
    // `revenue_breakdown` is that slice, `income_sources_12m` the whole
    // series, and `category_split` the order-line subset of the slice with
    // ratios over the order-line total (venue rental is not an order line
    // — see `dto::CategorySplitEntry`).
    let revenue_breakdown: Vec<IncomeSourceEntry> = income_rows
        .iter()
        .filter(|r| r.month == current_month_key)
        .map(|r| IncomeSourceEntry {
            source: r.source.clone(),
            gross_cents: r.gross_cents,
            orders_count: r.orders_count,
            units: r.units,
        })
        .collect();

    let order_line_total: i64 = revenue_breakdown
        .iter()
        .filter(|r| r.source != VENUE_RENTAL_SOURCE)
        .map(|r| r.gross_cents)
        .sum();
    let category_split: Vec<CategorySplitEntry> = revenue_breakdown
        .iter()
        .filter(|r| r.source != VENUE_RENTAL_SOURCE)
        .map(|r| CategorySplitEntry {
            source: r.source.clone(),
            gross_cents: r.gross_cents,
            ratio: safe_ratio(r.gross_cents, order_line_total),
        })
        .collect();

    let income_sources_12m: Vec<IncomeSourceMonthEntry> = income_rows
        .into_iter()
        .map(|r| IncomeSourceMonthEntry {
            month: r.month,
            source: r.source,
            gross_cents: r.gross_cents,
            orders_count: r.orders_count,
            units: r.units,
        })
        .collect();

    let payment_split: Vec<PaymentSplitEntry> = payment_rows
        .into_iter()
        .map(|(method, count)| PaymentSplitEntry { method, count })
        .collect();

    let attendance_distribution = bucket_entries(attendance_dist_rows);
    let age_distribution = bucket_entries(age_dist_rows);
    let tier_distribution = bucket_entries(tier_dist_rows);

    // `rate` = |上月活躍 ∩ 本月活躍| / |上月活躍| — `safe_ratio` renders the
    // empty-previous-month case as null (undefined), not 0.
    let retention: Vec<RetentionMonthRow> = retention_rows
        .into_iter()
        .map(|r| RetentionMonthRow {
            month: r.month,
            new_count: r.new_count,
            returning_count: r.returning_count,
            rate: safe_ratio(r.retained_count, r.prev_active_count),
        })
        .collect();

    let funnel = FunnelSection {
        trial_inquiries: funnel_row.trial_inquiries,
        new_enrolments: funnel_row.new_enrolments,
    };

    let weekday_load: Vec<WeekdayLoadEntry> = weekday_rows
        .into_iter()
        .map(|r| WeekdayLoadEntry { weekday: r.weekday, present_count: r.present_count })
        .collect();

    let venue_usage: Vec<VenueUsageEntry> = venue_rows
        .into_iter()
        .map(|r| VenueUsageEntry { venue: r.venue, minutes: r.minutes })
        .collect();

    AdminReportResponse {
        revenue: AdminRevenueSection { this_month_cents, last_month_cents, trend },
        kpis,
        revenue_breakdown,
        income_sources_12m,
        category_split,
        payment_split,
        attendance_distribution,
        age_distribution,
        tier_distribution,
        retention,
        funnel,
        weekday_load,
        venue_usage,
        members: AdminMembersSection { total, new_this_month, active },
        courses,
        coaches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// A fully-empty/zeroed `AdminReportInputs` other tests build on top of
    /// via struct-update syntax (`AdminReportInputs { field: ..., ..empty_inputs() }`),
    /// overriding just the field(s) they care about. Hand-written rather than
    /// `#[derive(Default)]` on the struct itself — this is test-local
    /// convenience, not a trait the production model types should carry.
    fn empty_inputs() -> AdminReportInputs {
        AdminReportInputs {
            trend_rows: vec![],
            member_stats: (0, 0, 0),
            course_rows: vec![],
            coach_rows: vec![],
            kpi: KpiRow {
                new_members_this: 0,
                new_members_last: 0,
                new_enrolments_this: 0,
                new_enrolments_last: 0,
                paid_orders_this: 0,
                paid_orders_last: 0,
                present_this: 0,
                absent_this: 0,
                present_last: 0,
                absent_last: 0,
            },
            income_rows: vec![],
            payment_rows: vec![],
            attendance_dist_rows: vec![],
            age_dist_rows: vec![],
            tier_dist_rows: vec![],
            retention_rows: vec![],
            funnel_row: FunnelRow { trial_inquiries: 0, new_enrolments: 0 },
            weekday_rows: vec![],
            venue_rows: vec![],
        }
    }

    // --- trend position index ---

    #[test]
    fn trend_position_index_picks_last_and_second_to_last_month() {
        let inputs = AdminReportInputs {
            trend_rows: vec![
                ("2026-05".to_string(), 1_000),
                ("2026-06".to_string(), 2_000),
                ("2026-07".to_string(), 3_000),
            ],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(report.revenue.this_month_cents, 3_000, "last row = this month");
        assert_eq!(report.revenue.last_month_cents, 2_000, "second-to-last row = last month");
    }

    #[test]
    fn trend_shorter_than_two_rows_does_not_panic() {
        // `revenue_trend` always returns exactly `TRAILING_WINDOW_MONTHS` rows
        // in real use, so a 0- or 1-row trend can never happen through the
        // real repository query — this is pure-core-only coverage for
        // `checked_sub`'s defensive guard, integration-unreachable.
        let one_row =
            AdminReportInputs { trend_rows: vec![("2026-07".to_string(), 500)], ..empty_inputs() };
        let report = assemble_admin_report(one_row, "2026-07");
        assert_eq!(report.revenue.this_month_cents, 500);
        assert_eq!(report.revenue.last_month_cents, 0, "no second-to-last row -> falls back to 0");

        let zero_rows = AdminReportInputs { trend_rows: vec![], ..empty_inputs() };
        let report = assemble_admin_report(zero_rows, "2026-07");
        assert_eq!(report.revenue.this_month_cents, 0);
        assert_eq!(report.revenue.last_month_cents, 0);
    }

    // --- revenue_breakdown current-month slice ---

    #[test]
    fn revenue_breakdown_slices_income_rows_to_current_month_only() {
        let inputs = AdminReportInputs {
            income_rows: vec![
                IncomeSourceRow {
                    month: "2026-06".into(),
                    source: "course".into(),
                    gross_cents: 100,
                    orders_count: 1,
                    units: 1,
                },
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "course".into(),
                    gross_cents: 200,
                    orders_count: 2,
                    units: 2,
                },
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "ticket".into(),
                    gross_cents: 300,
                    orders_count: 3,
                    units: 3,
                },
            ],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(report.revenue_breakdown.len(), 2, "only the 2026-07 rows");
        assert!(
            report.revenue_breakdown.iter().all(|r| r.gross_cents != 100),
            "the 2026-06 row must not leak in"
        );
        assert_eq!(report.income_sources_12m.len(), 3, "income_sources_12m keeps every month");
    }

    // --- category_split exclusion + ratio ---

    #[test]
    fn category_split_excludes_venue_rental_and_computes_ratio_over_order_line_total() {
        let inputs = AdminReportInputs {
            income_rows: vec![
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "course".into(),
                    gross_cents: 300,
                    orders_count: 1,
                    units: 1,
                },
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "ticket".into(),
                    gross_cents: 100,
                    orders_count: 1,
                    units: 1,
                },
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "venue_rental".into(),
                    gross_cents: 600,
                    orders_count: 1,
                    units: 1,
                },
            ],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert!(
            report.category_split.iter().all(|r| r.source != "venue_rental"),
            "venue rental excluded from category_split"
        );
        let course = report.category_split.iter().find(|r| r.source == "course").unwrap();
        assert_eq!(
            course.ratio,
            Some(0.75),
            "300 / (300+100) order-line total, venue's 600 excluded from the denominator"
        );
        let ticket = report.category_split.iter().find(|r| r.source == "ticket").unwrap();
        assert_eq!(ticket.ratio, Some(0.25));
    }

    // --- category_split zero-denominator, proving venue isn't in it ---

    #[test]
    fn category_split_ratio_is_none_when_order_line_total_is_zero_even_with_nonzero_venue_rental() {
        let inputs = AdminReportInputs {
            income_rows: vec![
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "course".into(),
                    gross_cents: 0,
                    orders_count: 0,
                    units: 0,
                },
                IncomeSourceRow {
                    month: "2026-07".into(),
                    source: "venue_rental".into(),
                    gross_cents: 500,
                    orders_count: 1,
                    units: 1,
                },
            ],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        // The order-line total is 0 (course=0, venue excluded) — every
        // category_split ratio must be None, not 0/NaN, even though a
        // nonzero-gross source (venue_rental) is present in
        // revenue_breakdown — proof that venue_rental never enters the
        // denominator.
        assert!(report.category_split.iter().all(|r| r.ratio.is_none()));
        let venue_row =
            report.revenue_breakdown.iter().find(|r| r.source == "venue_rental").unwrap();
        assert_eq!(venue_row.gross_cents, 500, "venue_rental still appears in revenue_breakdown");
    }

    // --- fill_rate divide-by-zero (unreachable via the DB CHECK) ---

    #[test]
    fn fill_rate_is_none_when_max_students_is_zero() {
        // `courses_max_students_pos CHECK (max_students > 0)` makes a real
        // `courses` row with `max_students = 0` unreachable via the DB —
        // this is pure-core-only coverage of `safe_ratio`'s divide-by-zero
        // guard as applied to `fill_rate`.
        let inputs = AdminReportInputs {
            course_rows: vec![AdminCourseRow {
                course_id: Uuid::now_v7(),
                name: "Zero Capacity".into(),
                enrolled: 0,
                max_students: 0,
                waitlist_count: 0,
            }],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(report.courses[0].fill_rate, None);
    }

    // --- coach attendance rate ---

    #[test]
    fn coach_attendance_rate_derives_from_present_and_absent_counts() {
        let inputs = AdminReportInputs {
            coach_rows: vec![
                AdminCoachRow {
                    coach_id: Uuid::now_v7(),
                    name: "Coach A".into(),
                    course_count: 2,
                    student_count: 3,
                    revenue_cents_12m: 10_000,
                    att_present: 3,
                    att_absent: 1,
                },
                AdminCoachRow {
                    coach_id: Uuid::now_v7(),
                    name: "Coach B".into(),
                    course_count: 1,
                    student_count: 1,
                    revenue_cents_12m: 0,
                    att_present: 0,
                    att_absent: 0,
                },
            ],
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(
            report.coaches[0].attendance_rate,
            Some(0.75),
            "3 present / (3 present + 1 absent)"
        );
        assert_eq!(report.coaches[1].attendance_rate, None, "no attendance data -> null, not 0");
    }

    // --- kpis + retention rate, zero denominator ---

    #[test]
    fn kpis_and_retention_rate_are_none_on_zero_denominator() {
        let inputs = AdminReportInputs {
            retention_rows: vec![RetentionRow {
                month: "2026-07".into(),
                new_count: 0,
                returning_count: 0,
                prev_active_count: 0,
                retained_count: 0,
            }],
            ..empty_inputs() // kpi is already all-zero
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(report.kpis.attendance_rate.this_month, None, "0 present + 0 absent -> null");
        assert_eq!(report.kpis.attendance_rate.last_month, None);
        assert_eq!(report.retention[0].rate, None, "empty previous month -> null, not 0");
    }

    // --- passthrough sections preserve values/order (incl. bucket_entries) ---

    #[test]
    fn passthrough_sections_preserve_field_values_and_order() {
        let inputs = AdminReportInputs {
            member_stats: (10, 2, 5),
            payment_rows: vec![("credit_card".to_string(), 3), ("line_pay".to_string(), 1)],
            attendance_dist_rows: vec![BucketCountRow { bucket: "gte_95".into(), count: 2 }],
            weekday_rows: vec![
                WeekdayLoadRow { weekday: 0, present_count: 4 },
                WeekdayLoadRow { weekday: 1, present_count: 7 },
            ],
            venue_rows: vec![
                VenueUsageRow { venue: "A".into(), minutes: 120 },
                VenueUsageRow { venue: "B".into(), minutes: 60 },
            ],
            funnel_row: FunnelRow { trial_inquiries: 9, new_enrolments: 3 },
            ..empty_inputs()
        };

        let report = assemble_admin_report(inputs, "2026-07");

        assert_eq!(report.members.total, 10);
        assert_eq!(report.members.new_this_month, 2);
        assert_eq!(report.members.active, 5);

        assert_eq!(report.payment_split.len(), 2);
        assert_eq!(report.payment_split[0].method, "credit_card");
        assert_eq!(report.payment_split[0].count, 3);
        assert_eq!(report.payment_split[1].method, "line_pay");
        assert_eq!(report.payment_split[1].count, 1);

        assert_eq!(report.attendance_distribution[0].bucket, "gte_95");
        assert_eq!(report.attendance_distribution[0].count, 2);

        assert_eq!(report.weekday_load[0].weekday, 0);
        assert_eq!(report.weekday_load[0].present_count, 4);
        assert_eq!(report.weekday_load[1].weekday, 1);
        assert_eq!(report.weekday_load[1].present_count, 7);

        assert_eq!(report.venue_usage[0].venue, "A");
        assert_eq!(report.venue_usage[0].minutes, 120);
        assert_eq!(report.venue_usage[1].venue, "B");
        assert_eq!(report.venue_usage[1].minutes, 60);

        assert_eq!(report.funnel.trial_inquiries, 9);
        assert_eq!(report.funnel.new_enrolments, 3);
    }

    // --- safe_ratio (migrated verbatim from service.rs) ---

    #[test]
    fn safe_ratio_divide_by_zero_is_none() {
        assert_eq!(safe_ratio(5, 0), None);
        assert_eq!(safe_ratio(0, 0), None);
    }

    #[test]
    fn safe_ratio_normal_case() {
        assert_eq!(safe_ratio(6, 8), Some(0.75));
        assert_eq!(safe_ratio(0, 5), Some(0.0));
    }
}
