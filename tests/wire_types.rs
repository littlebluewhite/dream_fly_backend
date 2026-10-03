//! Wire types (ADR-0016): every response DTO derives `ts_rs::TS`, and this
//! test is the single place that exports them. It writes the TypeScript into
//! `$CARGO_TARGET_TMPDIR/wire`, checks it, and compares it with the
//! committed `bindings/` dir the frontend consumes.
//!
//! - Check mode (default): fail when `bindings/` differs from what the DTOs
//!   generate today.
//! - Write mode: `WIRE_BINDINGS=write cargo test --test wire_types` replaces
//!   `bindings/` with the fresh output.
//!
//! The `wire_types!` list is the export list. The test fails when a listed
//! type references a type that is not listed, or when two listed types would
//! write the same `.ts` file (give one a `#[ts(rename = "...")]`). A second
//! test scans the DTO sources so a `Serialize` type left off the list fails
//! too, unless it is in `NOT_WIRE_TYPES`.
//! `i64` is exported as `number` (`with_large_int`): wire integers stay
//! below 2^53 (ADR-0016).

use std::any::TypeId;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use ts_rs::{Config, Dependency, ExportError, TS};

use dream_fly_backend::error;
use dream_fly_backend::extractors;
use dream_fly_backend::modules::{
    attendance, auth, bookings, cart, certificates, coaches, contact, coupons, courses,
    enrolments, leave, messages, notifications, orders, points, posts, products, reports,
    rewards, schedule, sessions, settings, subscriptions, users, venues, waitlist,
};

struct WireType {
    rust_name: &'static str,
    type_id: TypeId,
    output_path: PathBuf,
    dependencies: Vec<Dependency>,
    export_all: fn(&Config) -> Result<(), ExportError>,
}

impl WireType {
    fn of<T: TS + 'static>(cfg: &Config) -> Self {
        Self {
            rust_name: std::any::type_name::<T>(),
            type_id: TypeId::of::<T>(),
            output_path: T::output_path().expect("wire type must be exportable"),
            dependencies: T::dependencies(cfg),
            export_all: T::export_all,
        }
    }
}

macro_rules! wire_types {
    ($($ty:ty),+ $(,)?) => {
        fn wire_types(cfg: &Config) -> Vec<WireType> {
            vec![$(WireType::of::<$ty>(cfg)),+]
        }
    };
}

wire_types![
    attendance::dto::RosterEntryResponse,
    attendance::dto::MyStudentResponse,
    attendance::model::AttendanceStatus,
    attendance::model::StudentCourseBrief,
    auth::dto::AuthResponse,
    auth::dto::UserResponse, // TS: AuthUserResponse
    bookings::dto::BookingResponse,
    bookings::dto::PaginatedBookingsResponse,
    bookings::model::BookingStatus,
    cart::dto::CartItemResponse,
    cart::dto::CartResponse,
    cart::model::CartItemType,
    certificates::dto::ReportCardResponse,
    certificates::dto::CertificateResponse,
    coaches::dto::CoachResponse,
    coaches::dto::CoachDetailResponse,
    coaches::dto::CoachScheduleResponse,
    coaches::dto::ClockRecordResponse,
    contact::dto::InquiryResponse,
    contact::dto::InquiryListResponse,
    contact::model::InquiryStatus,
    contact::model::InquiryType,
    coupons::dto::CouponResponse,
    coupons::dto::CouponValidateResponse,
    coupons::dto::CouponListResponse,
    courses::dto::CourseScheduleSlotResponse,
    courses::dto::CourseResponse,
    courses::dto::CourseDetailResponse,
    courses::dto::CourseListResponse,
    courses::model::CourseLevel,
    enrolments::dto::EnrolmentResponse,
    enrolments::dto::MyEnrolmentResponse,
    enrolments::dto::AttendanceEntryResponse,
    enrolments::model::EnrolmentStatus,
    error::MessageResponse, // TS: MessageAck
    extractors::pagination::PageMeta,
    leave::dto::LeaveRequestResponse,
    leave::dto::AdminLeaveRequestResponse,
    leave::dto::LeaveRequestListResponse,
    leave::model::LeaveStatus,
    messages::dto::ConversationResponse,
    messages::dto::ConversationSummaryResponse,
    messages::dto::MessageResponse,
    messages::dto::MessageListResponse,
    messages::dto::MarkReadResponse,
    notifications::dto::NotificationResponse,
    notifications::dto::UnreadCountResponse,
    notifications::model::NotificationType,
    orders::dto::OrderResponse,
    orders::dto::OrderItemResponse,
    orders::dto::OrderListResponse,
    orders::dto::OrderSummary,
    orders::dto::AdminOrderSummary,
    orders::dto::AdminOrderListResponse,
    orders::model::OrderStatus,
    orders::model::OrderItemBrief,
    points::dto::LedgerEntryResponse,
    points::dto::PointsMeResponse,
    points::dto::PointsAdjustmentResponse,
    points::model::PointReason,
    posts::dto::PostResponse,
    posts::dto::PostDetailResponse,
    posts::dto::PostListResponse,
    posts::model::PostCategory,
    posts::model::PostStatus,
    products::dto::ProductResponse,
    products::dto::ProductListResponse,
    products::model::ProductType,
    reports::dto::RevenueMonthPoint,
    reports::dto::AdminRevenueSection,
    reports::dto::AdminMembersSection,
    reports::dto::AdminCourseReportRow,
    reports::dto::AdminCoachReportRow,
    reports::dto::BucketCountEntry,
    reports::dto::RetentionMonthRow,
    reports::dto::WeekdayLoadEntry,
    reports::dto::VenueUsageEntry,
    reports::dto::FunnelSection,
    reports::dto::MonthPair,
    reports::dto::RateMonthPair,
    reports::dto::KpisSection,
    reports::dto::IncomeSourceEntry,
    reports::dto::IncomeSourceMonthEntry,
    reports::dto::CategorySplitEntry,
    reports::dto::PaymentSplitEntry,
    reports::dto::AdminReportResponse,
    reports::dto::CoachReportResponse,
    reports::dto::MemberReportResponse,
    reports::dto::ActivityItem,
    reports::dto::ActivityResponse,
    rewards::dto::RewardResponse,
    rewards::dto::RewardListResponse,
    rewards::dto::RedeemResponse,
    rewards::dto::RedemptionResponse,
    rewards::dto::RedemptionListResponse,
    schedule::dto::TimeSlotResponse,
    schedule::dto::DaySchedule,
    schedule::model::SlotStatus,
    sessions::dto::CourseSessionResponse,
    sessions::dto::TodaySessionResponse,
    sessions::dto::MyScheduleEntryResponse,
    sessions::model::SessionStatus,
    settings::dto::SettingsResponse,
    subscriptions::dto::SubscriptionResponse,
    subscriptions::model::SubscriptionStatus,
    users::dto::UserResponse,
    users::dto::UserListResponse,
    venues::dto::VenueCategoryResponse,
    venues::dto::VenueResponse,
    venues::dto::VenueWithCategoryResponse,
    waitlist::dto::WaitlistResponse,
    waitlist::model::WaitlistStatus,
    // `NotificationResponse.metadata`, exported as `serde_json/JsonValue.ts`.
    serde_json::Value,
];

#[test]
fn bindings_match_response_dtos() {
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("wire");
    let _ = fs::remove_dir_all(&out_dir);
    let cfg = Config::new().with_large_int("number").with_out_dir(&out_dir);
    let types = wire_types(&cfg);

    // Two listed types writing one file would silently overwrite each other.
    let mut by_path: HashMap<&Path, &WireType> = HashMap::new();
    for t in &types {
        if let Some(prev) = by_path.insert(&t.output_path, t) {
            panic!(
                "{} and {} both export to {}; give one a #[ts(rename = \"...\")]",
                prev.rust_name,
                t.rust_name,
                t.output_path.display()
            );
        }
    }

    // Every type a listed type references must itself be listed. Matching by
    // Rust type also catches an unlisted type that shares a listed TS name.
    // A dependency on the type's own file is a self-reference: recursion, or
    // `serde_json::Value` delegating to ts-rs's private `JsonValue` type.
    let listed: HashMap<TypeId, &WireType> = types.iter().map(|t| (t.type_id, t)).collect();
    for t in &types {
        for dep in t.dependencies.iter().filter(|d| d.output_path != t.output_path) {
            assert!(
                listed.contains_key(&dep.type_id),
                "{} references `{}` ({}), which is not in the wire_types! list",
                t.rust_name,
                dep.ts_name,
                dep.output_path.display()
            );
        }
    }

    for t in &types {
        (t.export_all)(&cfg).unwrap_or_else(|e| panic!("export {}: {e}", t.rust_name));
    }

    let mut generated = read_tree(&out_dir);
    let mut expected: Vec<String> = types.iter().map(|t| slash_path(&t.output_path)).collect();
    expected.sort();
    let exported: Vec<&String> = generated.keys().collect();
    assert_eq!(
        exported,
        expected.iter().collect::<Vec<_>>(),
        "exported files differ from the wire_types! list"
    );

    generated.insert("index.ts".to_owned(), index_ts(&expected));

    let bindings = Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
    let committed = read_tree(&bindings);
    if committed == generated {
        return;
    }
    if std::env::var("WIRE_BINDINGS").as_deref() == Ok("write") {
        let _ = fs::remove_dir_all(&bindings);
        for (rel, content) in &generated {
            let path = bindings.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        return;
    }

    let mut report = Vec::new();
    for (rel, content) in &generated {
        match committed.get(rel) {
            None => report.push(format!("  missing  bindings/{rel}")),
            Some(c) if c != content => report.push(format!("  stale    bindings/{rel}")),
            Some(_) => {}
        }
    }
    for rel in committed.keys().filter(|rel| !generated.contains_key(*rel)) {
        report.push(format!("  extra    bindings/{rel}"));
    }
    panic!(
        "bindings/ is out of date with the response DTOs:\n{}\n\
         Regenerate with: WIRE_BINDINGS=write cargo test --test wire_types",
        report.join("\n")
    );
}

/// `Serialize` types in DTO sources that are deliberately not wire types.
const NOT_WIRE_TYPES: &[(&str, &str)] = &[
    ("coaches::dto::ScheduleEntry", "request entry of UpdateScheduleRequest"),
    ("schedule::dto::SlotEntry", "request entry of CreateSlotsRequest"),
    ("courses::dto::CourseScheduleSlotEntry", "request entry of Create/UpdateCourseRequest"),
];

/// Catches a response DTO nobody added to `wire_types!` (no other listed type
/// references it, so the dependency check cannot see it).
#[test]
fn every_serialize_dto_is_listed() {
    let cfg = Config::new().with_large_int("number");
    let listed: Vec<&str> = wire_types(&cfg).iter().map(|t| t.rust_name).collect();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<(PathBuf, String)> = vec![
        (src.join("error/mod.rs"), "error".into()),
        (src.join("extractors/pagination.rs"), "extractors::pagination".into()),
    ];
    for entry in fs::read_dir(src.join("modules")).unwrap() {
        let dir = entry.unwrap().path();
        let module = dir.file_name().unwrap().to_string_lossy().into_owned();
        if dir.join("dto.rs").exists() {
            files.push((dir.join("dto.rs"), format!("modules::{module}::dto")));
        }
    }
    for (file, module) in files {
        let text = fs::read_to_string(&file).unwrap();
        let mut serialize = false;
        for line in text.lines().map(str::trim) {
            if let Some(derives) = line.strip_prefix("#[derive(") {
                serialize = derives.trim_end_matches(")]").split(',').any(|d| d.trim() == "Serialize");
            } else if let Some(name) = ["pub struct ", "pub enum "].iter().find_map(|p| line.strip_prefix(p)) {
                let name = name.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap();
                let short = format!("{}::{name}", module.trim_start_matches("modules::"));
                let full = format!("dream_fly_backend::{module}::{name}");
                assert!(
                    !serialize || listed.contains(&full.as_str()) || NOT_WIRE_TYPES.iter().any(|(n, _)| *n == short),
                    "{full} derives Serialize but is not a wire type: derive ts_rs::TS and add it to \
                     wire_types!, or add it to NOT_WIRE_TYPES with a reason"
                );
                serialize = false;
            }
        }
    }
}

/// `export type { X } from "./X";` per exported file, sorted by path.
fn index_ts(paths: &[String]) -> String {
    let mut s = String::from(
        "// This file was generated by tests/wire_types.rs. Do not edit this file manually.\n",
    );
    for rel in paths {
        let module = rel.trim_end_matches(".ts");
        let name = module.rsplit('/').next().unwrap();
        s.push_str(&format!("export type {{ {name} }} from \"./{module}\";\n"));
    }
    s
}

/// Every file under `root`, keyed by its `/`-separated path relative to `root`.
fn read_tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = slash_path(path.strip_prefix(root).unwrap());
                out.insert(rel, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
