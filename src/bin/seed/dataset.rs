//! The dev/reporting dataset `dataset::run` writes — split out of
//! `bin/seed/main.rs` (the entry point: env/config/pool/migrate, then one
//! sampled `StudioNow` handed in here) so the dataset itself and
//! `SeedReport` live apart from process-level concerns; the `#[sqlx::test]`
//! invariants on `run` are in `bin/seed/tests.rs`.
//!
//! Each seed rule with its own interface is a sub-module: `accounts`
//! (account birth), `timeline` (paid purchases through checkout), `pending`
//! (pending contrast orders), `settlement` (points-tier settlement),
//! `attendance` (past-session marks) and `rental` (venue-rental slots and
//! bookings) — each module's doc states its own rules and idempotency. This
//! file keeps, top to bottom: the remaining per-table insert helpers (one
//! `// ---` banner per table), the reporting-dataset helpers, `SeedReport`,
//! then `run`, whose body composes the tables and sub-modules in
//! `// -- <table> --` steps. List the steps with
//! `grep -n '// -- ' src/bin/seed/dataset.rs` and read the one you need
//! rather than the whole file.
//!
//! Every insert here is `INSERT ... ON CONFLICT DO NOTHING` keyed on the
//! table's natural unique key (slug / code / a schedule slot's
//! course+day+start) — a bare `ON CONFLICT DO NOTHING` (no explicit target)
//! no-ops on ANY unique-constraint violation on that table, so this is safe
//! regardless of whether the unique constraint is a plain column or a
//! functional `LOWER(slug)` index. Where a table has no usable unique key
//! (rewards, contact_inquiries), idempotency is a SELECT-existence check on
//! the seed's deterministic natural key (name / `df-seed-…` email) instead;
//! paid orders, and the enrolments they carry, are keyed by checkout's own
//! idempotency (below). Running this binary twice must therefore leave row
//! counts unchanged on the second run.
//!
//! The 12-month reporting dataset (members / enrolments / orders / sessions /
//! attendance / venue-rental slots+bookings / inquiries) is **deterministic**:
//! every value derives from fixed index formulas over the run date — no
//! randomness — so re-running on the same day produces the identical set,
//! save the `DF-{YYYYMMDD}{suffix}` number checkout issues each paid order
//! (its identity across runs is its idempotency key, not that number).
//!
//! **Paid orders are bought through the real checkout.** Every paid,
//! completed or refunded seed order is a `SeedPurchase` (buyer, lines,
//! instant `ts`) that the seed purchase timeline (`timeline::buy_and_walk`)
//! places the way a member would: `cart::service::add_item` per line, then
//! `orders::service::checkout` under its `Idempotency-Key` at
//! `StudioNow { now: ts }`. Two sets of
//! purchases are merged and bought in `ts` order: the monthly product
//! orders (`DF-SEED-{YYYYMM}-{seq}`) and the 39 course purchases
//! (`DF-SEED-ENR-{i:02}-{k}`) that make up the enrolment dataset — every seed
//! enrolment is the one checkout makes for a bought course line, so every
//! one has its order. Pricing, order lines, the paid-order record and its
//! points ledger, the subscription grants and the enrolments are
//! checkout's own, all at `ts`; checkout also writes its outbox event,
//! notification and `order_idempotency` row, which keep the DB clock
//! (audit columns, ADR-0017). The timeline then walks completed and
//! refunded orders through `orders::service::update_order_status` — a
//! refund runs checkout's full compensation (`refund_clawback`, grant and
//! enrolment cancellation) — each straight after it is bought. A rerun
//! finds each key with
//! `orders::idempotency::replay` and buys nothing again, so it adds no
//! order, ledger, enrolment, outbox or notification row. Pending contrast
//! orders are the one bypass left on this path — checkout only creates
//! paid orders — and `pending::insert_pending_orders` writes them through
//! `orders::repository::create_order` / `create_order_items`.
//! `run` refuses, before any write, a database an older version of this
//! binary seeded (`refuse_legacy_seed_rows`): its seed enrolments and paid
//! `DF-SEED-…`-numbered orders were written directly, carry no key, and
//! would be bought a second time — reset the dev database first.
//!
//! `points_balance` is granted through `points::service::apply_delta_tx`,
//! never written to the column directly, so `points_balance ==
//! SUM(point_ledger.delta)` holds for every seeded user — but only on a
//! freshly migrated database: rows left over from an older run of this seed
//! (which wrote the column directly, with no backing ledger) are not
//! backfilled, an accepted legacy shape.
//! Every account is born through `accounts::upsert_account`; a named
//! account with opening points gets its whole balance via one
//! `LedgerDelta::admin_adjust` in the same transaction as the INSERT. The 24
//! reporting members are born with 0 points and instead earn theirs the way a
//! real member does: checkout writes a `checkout_earn` row at `paid_at` for
//! every paid seed order touching that member, and a refund a
//! `refund_clawback` on top at the refund instant — never the run instant;
//! once every order is bought, `settlement::settle_members` settles
//! **every** member (not just ones newly created this run) to its
//! points-tier target with one `admin_adjust(target − balance)`, every run
//! — the rules are in that module.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Datelike, Days, Duration, Months, NaiveDate, NaiveTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::cart::model::{CheckoutLine, LineTarget};
use dream_fly_backend::modules::contact::model::InquiryType;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::orders::fulfilment;
use dream_fly_backend::modules::orders::model::{OrderStatus, PAYMENT_METHODS};
use dream_fly_backend::modules::permissions::model::Role;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::modules::sessions::calendar::backfill_for_seed;
use dream_fly_backend::utils::password;
use dream_fly_backend::utils::studio_clock::StudioNow;

mod accounts;
mod attendance;
mod pending;
mod rental;
mod settlement;
mod timeline;

use accounts::{SeedAccount, upsert_account};
use pending::{PendingSeedOrder, insert_pending_orders};
use rental::seed_rentals;
use settlement::settle_members;
use timeline::{SeedPurchase, Walked};

/// Convert a fixed list of `&str` literals into the `Vec<String>` sqlx needs
/// to bind a Postgres `TEXT[]` column.
fn vs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// roles (accounts themselves are born in `accounts`)
// ---------------------------------------------------------------------------

/// Attach a role to a user — delegates to
/// `permissions::repository::assign_role` (idempotent, `ON CONFLICT
/// DO NOTHING`) via a pool-`acquire`d connection rather than hand-rolling the
/// `INSERT` (seed has no ambient transaction to reuse).
///
/// The returned `AccessDirty` witness is consumed with `assume_uncached`:
/// seed has no Redis connection to flush it through. This is a dev-only
/// exception, not the "provably uncached" case the witness is meant for —
/// on a seed re-run the target user may already have a cached entry — but a
/// stale entry just lives out its TTL (60s active flag / 15-minute roles)
/// before self-correcting.
async fn assign_role(db: &PgPool, user_id: Uuid, role: Role) -> anyhow::Result<()> {
    let role_name = role.as_str();
    let mut conn = db
        .acquire()
        .await
        .with_context(|| format!("acquire connection to assign role '{role_name}' to {user_id}"))?;
    permissions_repository::assign_role(&mut conn, user_id, role)
        .await
        .with_context(|| format!("assign role '{role_name}' to {user_id}"))?
        .assume_uncached();
    Ok(())
}

// ---------------------------------------------------------------------------
// coaches
// ---------------------------------------------------------------------------

struct CoachSeed {
    email: &'static str,
    user_name: &'static str,
    slug: &'static str,
    title: &'static str,
    bio: &'static str,
    specialties: &'static [&'static str],
    certifications: &'static [&'static str],
    display_order: i32,
}

/// Insert a coach row (idempotent on `slug`) and return its id.
async fn upsert_coach(db: &PgPool, user_id: Uuid, seed: &CoachSeed) -> anyhow::Result<Uuid> {
    sqlx::query(
        r#"
        INSERT INTO coaches (user_id, title, bio, specialties, certifications, is_active, display_order, slug, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, true, $6, $7, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(seed.title)
    .bind(seed.bio)
    .bind(vs(seed.specialties))
    .bind(vs(seed.certifications))
    .bind(seed.display_order)
    .bind(seed.slug)
    .execute(db)
    .await
    .with_context(|| format!("insert coach '{}'", seed.slug))?;

    sqlx::query_scalar("SELECT id FROM coaches WHERE slug = $1")
        .bind(seed.slug)
        .fetch_one(db)
        .await
        .with_context(|| format!("fetch id for coach '{}'", seed.slug))
}

// ---------------------------------------------------------------------------
// courses
// ---------------------------------------------------------------------------

struct CourseSeed {
    name: &'static str,
    slug: &'static str,
    level: &'static str,
    description: &'static str,
    duration_minutes: i32,
    price_cents: i64,
    max_students: i32,
    min_age: i32,
    max_age: i32,
    features: &'static [&'static str],
    category: &'static str,
    schedule_text: &'static str,
    is_highlighted: bool,
    coach_slug: &'static str,
    /// Structured weekly slots — `(day_of_week, start_time "HH:MM", end_time
    /// "HH:MM")`. `day_of_week` is 0=Sunday..6=Saturday (PostgreSQL
    /// `EXTRACT(DOW)` convention, see migration
    /// `20260706000001_course_schedule_slots_and_sessions.sql`). Not
    /// required to enumerate every day in `schedule_text` — just enough for
    /// dev's weekly schedule view to have real data.
    slots: &'static [(i16, &'static str, &'static str)],
    /// Venue name written onto every schedule slot. Matching a seed venue's
    /// `name` exactly is a deliberate display-layer alignment (so the string
    /// shown in reports reads like a real venue) — the reports module's
    /// venue-usage breakdown only `GROUP BY`s this string (snapshotted from
    /// `course_schedule_slots.venue` onto `course_sessions.venue` at
    /// materialization) directly; it never joins the `venues` table.
    venue: &'static str,
}

/// Insert a course (idempotent on `LOWER(slug)`).
async fn insert_course(db: &PgPool, seed: &CourseSeed, coach_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO courses (
            name, slug, level, description, duration_minutes, price_cents, max_students,
            min_age, max_age, features, coach_id, category, schedule_text, is_highlighted,
            created_at, updated_at
        )
        VALUES ($1, $2, $3::course_level, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(seed.name)
    .bind(seed.slug)
    .bind(seed.level)
    .bind(seed.description)
    .bind(seed.duration_minutes)
    .bind(seed.price_cents)
    .bind(seed.max_students)
    .bind(seed.min_age)
    .bind(seed.max_age)
    .bind(vs(seed.features))
    .bind(coach_id)
    .bind(seed.category)
    .bind(seed.schedule_text)
    .bind(seed.is_highlighted)
    .execute(db)
    .await
    .with_context(|| format!("insert course '{}'", seed.slug))?;
    Ok(())
}

/// Fetch a course's id by slug — used after `insert_course` (which doesn't
/// itself return the id) so its weekly schedule slots can be attached.
async fn course_id_by_slug(db: &PgPool, slug: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM courses WHERE slug = $1")
        .bind(slug)
        .fetch_one(db)
        .await
        .with_context(|| format!("fetch id for course '{slug}'"))
}

/// Insert one weekly schedule slot for a course (idempotent on the
/// `(course_id, day_of_week, start_time)` unique constraint). New installs
/// get `venue` written directly; rows that predate the venue column are
/// covered by the idempotent backfill UPDATE in `main` instead (the
/// ON CONFLICT no-op never touches an existing row).
async fn insert_course_schedule_slot(
    db: &PgPool,
    course_id: Uuid,
    day_of_week: i16,
    start_time: &str,
    end_time: &str,
    venue: &str,
) -> anyhow::Result<()> {
    let start = NaiveTime::parse_from_str(start_time, "%H:%M")
        .with_context(|| format!("parse seed start_time '{start_time}'"))?;
    let end = NaiveTime::parse_from_str(end_time, "%H:%M")
        .with_context(|| format!("parse seed end_time '{end_time}'"))?;

    sqlx::query(
        r#"
        INSERT INTO course_schedule_slots (id, course_id, day_of_week, start_time, end_time, venue, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(course_id)
    .bind(day_of_week)
    .bind(start)
    .bind(end)
    .bind(venue)
    .execute(db)
    .await
    .with_context(|| format!("insert course_schedule_slot for course {course_id}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// products
// ---------------------------------------------------------------------------

struct ProductSeed {
    name: &'static str,
    slug: &'static str,
    product_type: &'static str,
    description: &'static str,
    price_cents: i64,
    features: &'static [&'static str],
    is_highlighted: bool,
    badge: Option<&'static str>,
    valid_days: Option<i32>,
    session_count: Option<i32>,
}

/// Insert a product/plan (idempotent on `LOWER(slug)`). `stock` is left NULL
/// (unlimited) — tickets/memberships are entitlements, not finite inventory.
async fn insert_product(db: &PgPool, seed: &ProductSeed) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO products (
            name, slug, product_type, description, price_cents, features,
            is_highlighted, badge, stock, valid_days, session_count, is_active,
            created_at, updated_at
        )
        VALUES ($1, $2, $3::product_type, $4, $5, $6, $7, $8, NULL, $9, $10, true, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(seed.name)
    .bind(seed.slug)
    .bind(seed.product_type)
    .bind(seed.description)
    .bind(seed.price_cents)
    .bind(vs(seed.features))
    .bind(seed.is_highlighted)
    .bind(seed.badge)
    .bind(seed.valid_days)
    .bind(seed.session_count)
    .execute(db)
    .await
    .with_context(|| format!("insert product '{}'", seed.slug))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// coupons
// ---------------------------------------------------------------------------

/// Insert a coupon (idempotent on `code`). Always active, no expiry.
async fn insert_coupon(db: &PgPool, code: &str, discount_cents: i64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO coupons (code, discount_cents, is_active, expires_at, created_at)
        VALUES ($1, $2, true, NULL, NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(code)
    .bind(discount_cents)
    .execute(db)
    .await
    .with_context(|| format!("insert coupon '{code}'"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// rewards
// ---------------------------------------------------------------------------

struct RewardSeed {
    name: &'static str,
    description: &'static str,
    points_cost: i32,
    stock: Option<i32>,
    display_order: i32,
}

/// Insert a reward (idempotent on `name`). Unlike coupons/products, `rewards`
/// has no natural unique column to key an `ON CONFLICT` off of (see the
/// migration — brief's schema doesn't call for one), so idempotency here is
/// a plain existence check instead.
async fn insert_reward_if_absent(db: &PgPool, seed: &RewardSeed) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rewards WHERE name = $1)")
        .bind(seed.name)
        .fetch_one(db)
        .await
        .with_context(|| format!("check existing reward '{}'", seed.name))?;
    if exists {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO rewards (id, name, description, points_cost, stock, is_active, display_order, created_at, updated_at)
        VALUES (gen_random_uuid(), $1, $2, $3, $4, true, $5, NOW(), NOW())
        "#,
    )
    .bind(seed.name)
    .bind(seed.description)
    .bind(seed.points_cost)
    .bind(seed.stock)
    .bind(seed.display_order)
    .execute(db)
    .await
    .with_context(|| format!("insert reward '{}'", seed.name))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// posts (announcements)
// ---------------------------------------------------------------------------

struct PostSeed {
    title: &'static str,
    slug: &'static str,
    excerpt: &'static str,
    content: &'static str,
    days_ago: i64,
}

/// Insert a published announcement post (idempotent on `LOWER(slug)`).
async fn insert_post(
    db: &PgPool,
    author_id: Uuid,
    seed: &PostSeed,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let published_at: DateTime<Utc> = now - Duration::days(seed.days_ago);

    sqlx::query(
        r#"
        INSERT INTO posts (
            author_id, title, slug, content, excerpt, category, status,
            published_at, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, 'announcement'::post_category, 'published'::post_status, $6, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(author_id)
    .bind(seed.title)
    .bind(seed.slug)
    .bind(seed.content)
    .bind(seed.excerpt)
    .bind(published_at)
    .execute(db)
    .await
    .with_context(|| format!("insert post '{}'", seed.slug))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// venues
// ---------------------------------------------------------------------------

struct VenueSeed {
    name: &'static str,
    slug: &'static str,
    description: &'static str,
    features: &'static [&'static str],
}

/// Insert a venue (idempotent on `LOWER(slug)`). `category_id` is left NULL —
/// venue categories aren't part of this seed.
async fn insert_venue(db: &PgPool, seed: &VenueSeed) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO venues (name, slug, description, features, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, true, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(seed.name)
    .bind(seed.slug)
    .bind(seed.description)
    .bind(vs(seed.features))
    .execute(db)
    .await
    .with_context(|| format!("insert venue '{}'", seed.slug))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 12-month reporting dataset (members / enrolments / orders / attendance /
// venue rental / inquiries) — everything below is deterministic: fixed index
// formulas over the run date, no randomness. See the module doc for the
// per-table idempotency keys.
// ---------------------------------------------------------------------------

/// `date` at `HH:00:00` UTC. 04:00 UTC = 12:00 Asia/Taipei — mid-day keeps
/// the timestamp inside the same calendar day/month whether reports bucket
/// in UTC (dev `STUDIO_TIMEZONE`) or Asia/Taipei. Clamped to now: `today` is
/// the studio-tz date, which can run up to a day ahead of the UTC date, so a
/// today-dated seed row would otherwise land hours in the future.
fn at_utc(date: NaiveDate, hour: u32, now: DateTime<Utc>) -> DateTime<Utc> {
    date.and_hms_opt(hour, 0, 0)
        .expect("valid seed hour")
        .and_utc()
        .min(now)
}

/// Fetch a product's id by slug — mirrors `course_id_by_slug`, used to build
/// the order-item pool after `insert_product`.
async fn product_id_by_slug(db: &PgPool, slug: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM products WHERE slug = $1")
        .bind(slug)
        .fetch_one(db)
        .await
        .with_context(|| format!("fetch id for product '{slug}'"))
}

/// Fetch a venue's id by slug — used to attach rental time slots.
async fn venue_id_by_slug(db: &PgPool, slug: &str) -> anyhow::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM venues WHERE slug = $1")
        .bind(slug)
        .fetch_one(db)
        .await
        .with_context(|| format!("fetch id for venue '{slug}'"))
}

/// One contact inquiry — trial rows carry the structured `metadata` the
/// mobile 試上 flow assembles (category/student_age/preferred_day/
/// preferred_slot/parent_name/parent_phone/student_name/note).
struct InquirySeed {
    email: String,
    name: String,
    phone: String,
    subject: String,
    message: &'static str,
    inquiry_type: &'static str,
    metadata: Option<serde_json::Value>,
    created_at: DateTime<Utc>,
}

/// Insert a contact inquiry (idempotent by existence check on the seed's
/// `df-seed-…` email — the table has no unique column at all).
async fn insert_inquiry_if_absent(db: &PgPool, seed: &InquirySeed) -> anyhow::Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM contact_inquiries WHERE email = $1)")
            .bind(&seed.email)
            .fetch_one(db)
            .await
            .with_context(|| format!("check existing inquiry '{}'", seed.email))?;
    if exists {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO contact_inquiries (id, name, email, phone, subject, message, status, inquiry_type, metadata, assigned_to, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, 'new'::inquiry_status, $7, $8, NULL, $9, $9)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(&seed.name)
    .bind(&seed.email)
    .bind(&seed.phone)
    .bind(&seed.subject)
    .bind(seed.message)
    .bind(seed.inquiry_type)
    .bind(&seed.metadata)
    .bind(seed.created_at)
    .execute(db)
    .await
    .with_context(|| format!("insert inquiry '{}'", seed.email))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// verification helper — collected at the end of every run, printed by
// `bin/seed/main.rs` under the `-- row counts --` header.
// ---------------------------------------------------------------------------

async fn collect_row_counts(db: &PgPool) -> anyhow::Result<Vec<(&'static str, i64)>> {
    // Literal (label, query) pairs — kept as `&'static str` rather than a
    // `format!`-built string so sqlx's `SqlSafeStr` compile-time check (no
    // dynamic SQL strings) is satisfied without an `AssertSqlSafe` escape
    // hatch.
    const QUERIES: [(&str, &str); 20] = [
        ("users", "SELECT COUNT(*) FROM users"),
        ("coaches", "SELECT COUNT(*) FROM coaches"),
        ("courses", "SELECT COUNT(*) FROM courses"),
        ("products", "SELECT COUNT(*) FROM products"),
        ("coupons", "SELECT COUNT(*) FROM coupons"),
        ("rewards", "SELECT COUNT(*) FROM rewards"),
        ("posts", "SELECT COUNT(*) FROM posts"),
        ("venues", "SELECT COUNT(*) FROM venues"),
        ("orders", "SELECT COUNT(*) FROM orders"),
        ("order_items", "SELECT COUNT(*) FROM order_items"),
        (
            "order_idempotency",
            "SELECT COUNT(*) FROM order_idempotency",
        ),
        ("subscriptions", "SELECT COUNT(*) FROM subscriptions"),
        ("enrolments", "SELECT COUNT(*) FROM enrolments"),
        (
            "attendance_records",
            "SELECT COUNT(*) FROM attendance_records",
        ),
        ("course_sessions", "SELECT COUNT(*) FROM course_sessions"),
        ("time_slots", "SELECT COUNT(*) FROM time_slots"),
        ("bookings", "SELECT COUNT(*) FROM bookings"),
        (
            "contact_inquiries",
            "SELECT COUNT(*) FROM contact_inquiries",
        ),
        ("notifications", "SELECT COUNT(*) FROM notifications"),
        ("events_outbox", "SELECT COUNT(*) FROM events_outbox"),
    ];
    let mut row_counts = Vec::with_capacity(QUERIES.len());
    for (table, sql) in QUERIES {
        let n: i64 = sqlx::query_scalar(sql).fetch_one(db).await?;
        row_counts.push((table, n));
    }
    Ok(row_counts)
}

/// What one `dataset::run` finished with — `bin/seed/main.rs` prints
/// `row_counts` under the `-- row counts --` header; `#[sqlx::test]`s assert
/// on both fields directly instead of scraping stdout.
pub struct SeedReport {
    pub row_counts: Vec<(&'static str, i64)>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read by bin/seed/tests.rs; run() already prints the [points] line"
        )
    )]
    pub settled_members: usize,
}

/// Bail if an older version of this binary seeded `db`: it wrote the seed
/// members' enrolments directly (`order_id IS NULL`) and its paid orders
/// directly under `DF-SEED-…` order numbers, with no idempotency key —
/// checkout would buy all of them a second time. Scoped to seed rows: an
/// enrolment a developer made by hand for another account doesn't trip it,
/// and neither does a `DF-SEED-…` pending order an admin cancelled (the
/// legal Pending → Cancelled edge) — only a status that was paid does.
async fn refuse_legacy_seed_rows(db: &PgPool) -> anyhow::Result<()> {
    let (orderless_enrolments, direct_orders): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM enrolments e JOIN users u ON u.id = e.user_id \
            WHERE u.email LIKE 'seed-member-%@dreamfly.tw' AND e.order_id IS NULL), \
           (SELECT COUNT(*) FROM orders \
            WHERE order_number LIKE 'DF-SEED-%' \
              AND status NOT IN ('pending'::order_status, 'cancelled'::order_status))",
    )
    .fetch_one(db)
    .await
    .context("check for rows an older seed wrote")?;
    if orderless_enrolments > 0 || direct_orders > 0 {
        anyhow::bail!(
            "this database was seeded by an older seed ({orderless_enrolments} seed-member \
             enrolments without an order, {direct_orders} paid DF-SEED-… orders); \
             reset the dev DB (drop & recreate) and rerun"
        );
    }
    Ok(())
}

/// The seed dataset itself — everything `bin/seed/main.rs`'s entry point
/// (env/config/pool/migrate, one sampled `at: StudioNow`) hands off to.
/// Idempotent and safe to re-run: see the module doc for the per-table
/// idempotency keys and the points-ledger settlement shape.
pub async fn run(db: &PgPool, at: StudioNow) -> anyhow::Result<SeedReport> {
    refuse_legacy_seed_rows(db).await?;

    // Hash each distinct dev plaintext once, up front — Admin#2026 (admin),
    // Member#2026 (test member + all 24 reporting members) and Coach#2026
    // (all 4 coaches) — instead of once per `upsert_account`
    // call: 7 Argon2 hashes down to 3 per run.
    let admin_hash = password::hash_password("Admin#2026".to_string())
        .await
        .map_err(|e| anyhow::anyhow!("hashing seed admin password: {e}"))?;
    let member_hash = password::hash_password("Member#2026".to_string())
        .await
        .map_err(|e| anyhow::anyhow!("hashing seed member password: {e}"))?;
    let coach_hash = password::hash_password("Coach#2026".to_string())
        .await
        .map_err(|e| anyhow::anyhow!("hashing seed coach password: {e}"))?;

    // Every seed account joins on the first of the month one month before
    // the oldest `[orders]` month (11 months back), so each user predates
    // every seeded row that references it — its orders, enrolments, ledger
    // rows and bookings, the admin's posts and attendance marks. A re-run
    // keeps an existing row's `created_at` (`ON CONFLICT DO NOTHING`).
    let joined_at = at_utc(
        at.today()
            .with_day(1)
            .expect("day 1 always valid")
            .checked_sub_months(Months::new(12))
            .expect("valid seed join month"),
        4,
        at.now,
    );

    // -- admin -----------------------------------------------------------
    let admin_id = upsert_account(
        db,
        &SeedAccount {
            email: "admin@dreamfly.tw",
            name: "系統管理員",
            password_hash: &admin_hash,
            birth_date: None,
            points: 0,
            joined_at,
        },
        at.now,
    )
    .await?;
    assign_role(db, admin_id, Role::Admin).await?;
    println!("[users]    admin ready: admin@dreamfly.tw / Admin#2026");

    // -- test member -------------------------------------------------------
    let member_id = upsert_account(
        db,
        &SeedAccount {
            email: "member@dreamfly.tw",
            name: "測試會員",
            password_hash: &member_hash,
            birth_date: None,
            points: 1250,
            joined_at,
        },
        at.now,
    )
    .await?;
    assign_role(db, member_id, Role::Member).await?;
    println!("[users]    member ready: member@dreamfly.tw / Member#2026 (points_balance=1250)");

    // -- coaches -----------------------------------------------------------
    let coach_seeds: [CoachSeed; 4] = [
        CoachSeed {
            email: "coach1@dreamfly.tw",
            user_name: "王教練",
            slug: "wang",
            title: "資深體操教練",
            bio: "擁有 10 年競技體操教學經驗，專長兒童與青少年體操啟蒙訓練。",
            specialties: &["競技體操", "地板動作", "兒童體適能"],
            certifications: &["中華民國體操協會 C 級教練證", "運動防護員證照"],
            display_order: 0,
        },
        CoachSeed {
            email: "coach2@dreamfly.tw",
            user_name: "李教練",
            slug: "li",
            title: "啦啦隊主教練",
            bio: "曾帶領校隊獲得全國啦啦隊錦標賽冠軍，擅長技巧編排與團隊默契訓練。",
            specialties: &["啦啦隊技巧", "團體隊形", "彩帶舞"],
            certifications: &["中華民國啦啦隊協會教練證", "美國 USASF 認證教練"],
            display_order: 1,
        },
        CoachSeed {
            email: "coach3@dreamfly.tw",
            user_name: "張教練",
            slug: "zhang",
            title: "跑酷教練",
            bio: "熱愛極限運動，致力於推廣安全跑酷訓練，帶領學員突破自我。",
            specialties: &["跑酷基礎動作", "障礙訓練", "彈跳技巧"],
            certifications: &["國際跑酷聯盟（Parkour UK）初階教練證"],
            display_order: 2,
        },
        CoachSeed {
            email: "coach4@dreamfly.tw",
            user_name: "陳教練",
            slug: "chen",
            title: "幼兒體能教練",
            bio: "專注 3-6 歲幼兒體能發展，透過遊戲化教學培養孩子的協調性與自信心。",
            specialties: &["幼兒體能啟蒙", "感覺統合訓練", "親子律動"],
            certifications: &["幼兒體適能指導員證照", "感覺統合訓練師證照"],
            display_order: 3,
        },
    ];

    let mut coach_ids: HashMap<&'static str, Uuid> = HashMap::new();
    for seed in &coach_seeds {
        let user_id = upsert_account(
            db,
            &SeedAccount {
                email: seed.email,
                name: seed.user_name,
                password_hash: &coach_hash,
                birth_date: None,
                points: 0,
                joined_at,
            },
            at.now,
        )
        .await?;
        assign_role(db, user_id, Role::Coach).await?;
        let coach_id = upsert_coach(db, user_id, seed).await?;
        coach_ids.insert(seed.slug, coach_id);
        println!(
            "[coaches]  {} ready: {} / Coach#2026",
            seed.user_name, seed.email
        );
    }

    // -- courses -------------------------------------------------------------
    let course_seeds: [CourseSeed; 6] = [
        CourseSeed {
            name: "兒童體操啟蒙班",
            slug: "kids-gymnastics-beginner",
            level: "beginner",
            description: "專為 4-7 歲幼童設計的體操啟蒙課程，透過遊戲化教學建立基礎柔軟度與翻滾動作，培養孩子的專注力與自信心。",
            duration_minutes: 60,
            price_cents: 280_000,
            max_students: 12,
            min_age: 4,
            max_age: 7,
            features: &["基礎柔軟度訓練", "翻滾動作啟蒙", "專注力與自信心培養"],
            category: "體操",
            schedule_text: "週二、四 16:00-17:00",
            is_highlighted: true,
            coach_slug: "wang",
            slots: &[(2, "16:00", "17:00"), (4, "16:00", "17:00")],
            venue: "地板體操區",
        },
        CourseSeed {
            name: "競技體操進階班",
            slug: "gymnastics-advanced",
            level: "advanced",
            description: "適合已具備基礎動作能力的學員，強化各項體操項目的專項技巧，為參加校際及全國賽事做準備。",
            duration_minutes: 90,
            price_cents: 450_000,
            max_students: 10,
            min_age: 8,
            max_age: 15,
            features: &["競技動作組合訓練", "體操項目專項強化", "比賽選手培訓"],
            category: "體操",
            schedule_text: "週一、三、五 19:00-20:30",
            is_highlighted: false,
            coach_slug: "wang",
            slots: &[(1, "19:00", "20:30"), (3, "19:00", "20:30")],
            venue: "地板體操區",
        },
        CourseSeed {
            name: "啦啦隊基礎技巧班",
            slug: "cheer-basics",
            level: "beginner",
            description: "從零開始學習啦啦隊基本動作、隊形與口號帶動，適合喜愛團體活動、想挑戰自我的學員。",
            duration_minutes: 90,
            price_cents: 320_000,
            max_students: 16,
            min_age: 6,
            max_age: 12,
            features: &["基本隊形站位", "彩帶與口號帶動", "團隊合作精神培養"],
            category: "啦啦",
            schedule_text: "週二、四 19:00-20:30",
            is_highlighted: true,
            coach_slug: "li",
            slots: &[(2, "19:00", "20:30"), (4, "19:00", "20:30")],
            venue: "彈翻床區",
        },
        CourseSeed {
            name: "啦啦隊競技選手班",
            slug: "cheer-competitive",
            level: "advanced",
            description: "針對有意參加競賽的選手設計，訓練技巧堆疊、拋接與競賽編排，全面提升團隊競技水準。",
            duration_minutes: 120,
            price_cents: 420_000,
            max_students: 12,
            min_age: 10,
            max_age: 18,
            features: &["技巧堆疊與拋接", "競賽套路編排", "體能與協調性強化"],
            category: "啦啦",
            schedule_text: "週三、五 19:00-21:00",
            is_highlighted: false,
            coach_slug: "li",
            slots: &[(3, "19:00", "21:00"), (5, "19:00", "21:00")],
            venue: "空中技巧區",
        },
        CourseSeed {
            name: "跑酷體驗班",
            slug: "parkour-intro",
            level: "beginner",
            description: "透過安全的環境與循序漸進的教學，學習跑酷基礎動作、安全落地與障礙越過技巧。",
            duration_minutes: 90,
            price_cents: 300_000,
            max_students: 10,
            min_age: 8,
            max_age: 16,
            features: &["安全落地技巧", "基礎跳躍與翻越", "障礙訓練體驗"],
            category: "跑酷",
            schedule_text: "週六 10:00-11:30",
            is_highlighted: false,
            coach_slug: "zhang",
            slots: &[(6, "10:00", "11:30")],
            venue: "彈翻床區",
        },
        CourseSeed {
            name: "幼兒體能律動班",
            slug: "toddler-fitness",
            level: "beginner",
            description: "以遊戲與音樂律動為主軸，幫助 3-6 歲幼兒發展大肌肉動作能力與感覺統合，增進親子互動。",
            duration_minutes: 60,
            price_cents: 250_000,
            max_students: 8,
            min_age: 3,
            max_age: 6,
            features: &["感覺統合遊戲", "親子互動律動", "大肌肉發展訓練"],
            category: "幼兒",
            schedule_text: "週三、五 10:00-11:00",
            is_highlighted: false,
            coach_slug: "chen",
            slots: &[(3, "10:00", "11:00"), (5, "10:00", "11:00")],
            venue: "幼兒遊戲區",
        },
    ];

    for seed in &course_seeds {
        let coach_id = *coach_ids
            .get(seed.coach_slug)
            .ok_or_else(|| anyhow::anyhow!("unknown coach slug '{}'", seed.coach_slug))?;
        insert_course(db, seed, coach_id).await?;
    }
    println!("[courses]  {} courses ready", course_seeds.len());

    // -- course weekly schedule slots -----------------------------------------
    // `course_ids` is collected in course_seeds order — the deterministic
    // index base for enrolments / order lines / attendance below.
    let mut course_ids: Vec<Uuid> = Vec::with_capacity(course_seeds.len());
    let mut slot_count = 0usize;
    for seed in &course_seeds {
        let course_id = course_id_by_slug(db, seed.slug).await?;
        course_ids.push(course_id);
        for (day_of_week, start_time, end_time) in seed.slots {
            insert_course_schedule_slot(
                db,
                course_id,
                *day_of_week,
                start_time,
                end_time,
                seed.venue,
            )
            .await?;
            slot_count += 1;
        }
        // Idempotent venue backfill for installs whose slots predate the
        // venue column (the slot INSERT above no-ops on existing rows).
        sqlx::query(
            "UPDATE course_schedule_slots SET venue = $2 WHERE course_id = $1 AND venue IS NULL",
        )
        .bind(course_id)
        .bind(seed.venue)
        .execute(db)
        .await
        .with_context(|| format!("backfill slot venue for course '{}'", seed.slug))?;
    }
    println!("[courses]  {slot_count} weekly schedule slots ready (venues backfilled)");

    // -- products / plans ----------------------------------------------------
    // Array order is load-bearing for the order seeding below: the order-item
    // pool cycles `product_seeds[g % 7]`, so all four product_types
    // (ticket / membership / course_package / merchandise) appear in orders.
    let product_seeds: [ProductSeed; 7] = [
        ProductSeed {
            name: "單堂體驗券",
            slug: "single-trial-ticket",
            product_type: "ticket",
            description: "第一次來夢想飛翔嗎？單堂體驗券讓你親自體驗課程內容，不需長期承諾即可入門。",
            price_cents: 35_000,
            features: &["適用所有常規課程", "無使用期限壓力", "體驗後可洽詢升級方案"],
            is_highlighted: false,
            badge: None,
            valid_days: None,
            session_count: Some(1),
        },
        ProductSeed {
            name: "十堂票",
            slug: "ten-session-ticket",
            product_type: "ticket",
            description: "彈性堂票方案，十堂彈性使用，適合想固定練習但無法配合長期課表的學員。",
            price_cents: 300_000,
            features: &["十堂彈性使用", "可用於任何常規課程", "無使用期限壓力"],
            is_highlighted: false,
            badge: None,
            valid_days: None,
            session_count: Some(10),
        },
        ProductSeed {
            name: "月票",
            slug: "monthly-pass",
            product_type: "membership",
            description: "一個月內不限次數參加常規課程，適合想密集訓練的學員。",
            price_cents: 320_000,
            features: &[
                "30 天不限次數上課",
                "可預約所有常規課程",
                "隨時開始，效期 30 天",
            ],
            is_highlighted: false,
            badge: None,
            valid_days: Some(30),
            session_count: None,
        },
        ProductSeed {
            name: "季票",
            slug: "quarterly-pass",
            product_type: "membership",
            description: "三個月完整訓練週期，價格更優惠，是中長期學習的最佳選擇。",
            price_cents: 880_000,
            features: &[
                "90 天不限次數上課",
                "比月票更優惠的平均月費",
                "適合中長期訓練規劃",
            ],
            is_highlighted: true,
            badge: Some("最超值"),
            valid_days: Some(90),
            session_count: None,
        },
        ProductSeed {
            name: "年卡",
            slug: "annual-pass",
            product_type: "membership",
            description: "全年度不限次數訓練，享有最優惠的長期方案，是最具承諾但最划算的選擇。",
            price_cents: 3_000_000,
            features: &[
                "365 天不限次數上課",
                "全年最優惠平均月費",
                "專屬會員生日禮遇",
            ],
            is_highlighted: false,
            badge: None,
            valid_days: Some(365),
            session_count: None,
        },
        ProductSeed {
            name: "兒童體操 12 堂課程包",
            slug: "gym-12-session-package",
            product_type: "course_package",
            description: "為固定練習的孩子設計的 12 堂課程包，效期四個月內彈性排課，循序累積基礎動作。",
            price_cents: 330_000,
            features: &["12 堂彈性排課", "效期 120 天", "適用兒童體操系列課程"],
            is_highlighted: false,
            badge: None,
            valid_days: Some(120),
            session_count: Some(12),
        },
        ProductSeed {
            name: "學苑紀念 T 恤",
            slug: "academy-tee",
            product_type: "merchandise",
            description: "夢想飛翔學苑限定紀念 T 恤，吸濕排汗布料，訓練與日常都好穿。",
            price_cents: 68_000,
            features: &["吸濕排汗布料", "學苑限定圖樣", "兒童與成人尺寸"],
            is_highlighted: false,
            badge: None,
            valid_days: None,
            session_count: None,
        },
    ];

    for seed in &product_seeds {
        insert_product(db, seed).await?;
    }
    println!("[products] {} products/plans ready", product_seeds.len());

    // -- coupons ---------------------------------------------------------
    let coupon_seeds: [(&str, i64); 3] = [
        ("DREAMFLY100", 10_000),
        ("NEWYEAR500", 50_000),
        ("WELCOME50", 5_000),
    ];
    for (code, discount_cents) in coupon_seeds {
        insert_coupon(db, code, discount_cents).await?;
    }
    println!("[coupons]  {} coupons ready", coupon_seeds.len());

    // -- rewards (points redemption catalog) ----------------------------------
    let reward_seeds: [RewardSeed; 3] = [
        RewardSeed {
            name: "夢想飛翔運動毛巾",
            description: "館內限定運動毛巾，吸濕排汗，訓練必備。",
            points_cost: 50,
            stock: None,
            display_order: 0,
        },
        RewardSeed {
            name: "免費體驗課程一堂",
            description: "可折抵任一常規課程的單堂體驗名額。",
            points_cost: 150,
            stock: Some(10),
            display_order: 1,
        },
        RewardSeed {
            name: "教練簽名限量海報",
            description: "館內教練團簽名海報，數量有限，換完為止。",
            points_cost: 300,
            stock: Some(2),
            display_order: 2,
        },
    ];
    for seed in &reward_seeds {
        insert_reward_if_absent(db, seed).await?;
    }
    println!("[rewards]  {} rewards ready", reward_seeds.len());

    // -- posts (announcements) ------------------------------------------------
    let post_seeds: [PostSeed; 3] = [
        PostSeed {
            title: "夢想飛翔館全新體操課程開跑！",
            slug: "new-gymnastics-program-launch",
            excerpt: "全新兒童體操啟蒙課程正式開放報名，即日起加入享有限時優惠。",
            content: "夢想飛翔體操館很高興宣布，全新的兒童體操啟蒙課程已正式開放報名！本課程專為 4-7 歲幼童設計，由資深教練親自帶領，透過遊戲化教學建立基礎柔軟度與翻滾動作，同時培養孩子的專注力與自信心。即日起完成報名並繳費的學員，可享有限時優惠方案，名額有限，歡迎把握機會，一起陪伴孩子探索體操的樂趣！",
            days_ago: 1,
        },
        PostSeed {
            title: "夏季啦啦隊選手訓練營報名開始",
            slug: "summer-cheer-training-camp",
            excerpt: "為期四週的密集訓練營，適合準備參加競賽的啦啦隊選手報名參加。",
            content: "為了幫助有志於競技啦啦隊的學員做好賽季準備，夢想飛翔體操館將於本季推出為期四週的密集訓練營。課程內容涵蓋技巧堆疊、拋接動作與競賽套路編排，並由具備豐富比賽經驗的教練團隊親自指導。訓練營名額有限，適合已具備基礎技巧的學員報名參加，欲了解詳情或報名，歡迎洽詢櫃檯人員。",
            days_ago: 3,
        },
        PostSeed {
            title: "館內設施升級公告：全新空中技巧區啟用",
            slug: "facility-upgrade-aerial-zone",
            excerpt: "全新空中技巧訓練區正式啟用，提供更安全完善的練習環境。",
            content: "為提供學員更完善的訓練環境，夢想飛翔體操館全新打造的空中技巧訓練區已正式啟用！新場地配備專業安全吊掛系統與防護氣墊，並由專人在場指導陪同，讓學員能夠安心挑戰更高難度的動作。歡迎所有學員親自體驗全新升級的訓練空間。",
            days_ago: 7,
        },
    ];

    for seed in &post_seeds {
        insert_post(db, admin_id, seed, at.now).await?;
    }
    println!("[posts]    {} announcement posts ready", post_seeds.len());

    // -- venues ------------------------------------------------------------
    let venue_seeds: [VenueSeed; 4] = [
        VenueSeed {
            name: "彈翻床區",
            slug: "trampoline-zone",
            description: "配備專業彈翻床設備，提供彈跳與空翻動作訓練的安全場地，適合各程度學員使用。",
            features: &["專業彈翻床設備", "四周防護軟墊", "挑高天花板設計"],
        },
        VenueSeed {
            name: "地板體操區",
            slug: "floor-gymnastics-zone",
            description: "標準地板體操訓練區，適合基礎動作練習與體操項目專項訓練。",
            features: &["國際標準地墊", "整面鏡牆設計", "恆溫恆濕環境控制"],
        },
        VenueSeed {
            name: "空中技巧區",
            slug: "aerial-skills-zone",
            description: "提供吊環、繩索等空中技巧訓練設備，適合進階學員挑戰高難度動作。",
            features: &["專業安全吊掛系統", "防護氣墊", "專人指導陪同"],
        },
        VenueSeed {
            name: "幼兒遊戲區",
            slug: "kids-play-zone",
            description: "專為幼兒設計的軟式遊戲區，安全開放的空間讓孩子自由探索體能發展。",
            features: &["軟式地墊與器材", "安全防護邊角", "家長休息等候區"],
        },
    ];

    for seed in &venue_seeds {
        insert_venue(db, seed).await?;
    }
    println!("[venues]   {} venues ready", venue_seeds.len());

    // =====================================================================
    // 12-month deterministic reporting dataset. `today` anchors every
    // formula below; all indexes are fixed arithmetic — same-day re-runs
    // produce the identical set (and the per-table idempotency keys make
    // re-runs no-ops regardless).
    // =====================================================================
    let today: NaiveDate = at.today();

    // -- members ×24 -------------------------------------------------------
    // Age buckets (6-12 / 13-17 / 18-25 / 26-40) rotate on (i-1)%4 — six
    // members each, so the age-distribution report always has every bucket.
    // Points tiers (`PointsTier`) block on (i-1)/6 — six members each, see
    // `settlement::member_points_target` (member `i` is `member_ids[i - 1]`).
    // Different index bases so age and tier decorrelate. `member_hash` is the same Member#2026 hash the test
    // member above shares — hashed once, up front (see `run`'s precompute).
    let mut member_ids: Vec<Uuid> = Vec::with_capacity(24);
    for i in 1..=24usize {
        let age: u32 = match (i - 1) % 4 {
            0 => 7 + (i as u32 % 5),   //  7-11 → 6-12 bucket
            1 => 13 + (i as u32 % 5),  // 13-17
            2 => 18 + (i as u32 % 7),  // 18-24 → 18-25 bucket
            _ => 27 + (i as u32 % 12), // 27-38 → 26-40 bucket
        };
        // Subtract `age` years plus 1-9 months so the person stays `age`
        // years old for months after seeding (never straddles a bucket edge
        // the day after a run).
        let birth_date = today
            .checked_sub_months(Months::new(age * 12 + 1 + (i as u32 % 9)))
            .expect("valid seed birth_date");
        let email = format!("seed-member-{i:02}@dreamfly.tw");
        let name = format!("示範會員{i:02}");
        let user_id = upsert_account(
            db,
            &SeedAccount {
                email: &email,
                name: &name,
                password_hash: &member_hash,
                birth_date: Some(birth_date),
                points: 0,
                joined_at,
            },
            at.now,
        )
        .await?;
        assign_role(db, user_id, Role::Member).await?;
        member_ids.push(user_id);
    }
    println!("[members]  24 seed members ready (seed-member-01..24@dreamfly.tw / Member#2026)");

    // -- orders + order_items (~120 across 12 months) -----------------------
    // Identity: `DF-SEED-{YYYYMM}-{seq:02}` — the checkout idempotency key of
    // a paid/completed/refunded `SeedPurchase`, the order_number of a
    // pending one. Month `abs` (absolute month, `year * 12 + month0`) gets
    // 8 + ((abs*7+3)%5) orders; every month carries one refunded (seq 4) and
    // one pending (seq 7) contrast row. `g`, count, member and status
    // derive from `(abs, seq)` alone, never from how many months back the
    // month sits: `order_idempotency` is per (buyer, key), so a key whose
    // buyer shifted as the 12-month window slides would miss its replay and
    // be bought again for another member. The purchases are bought together
    // with the course purchases below.
    let mut product_ids: Vec<Uuid> = Vec::with_capacity(product_seeds.len());
    for seed in &product_seeds {
        product_ids.push(product_id_by_slug(db, seed.slug).await?);
    }
    // seq-keyed payment_method weights: credit_card ×5, line_pay ×2,
    // atm / jkopay / cash ×1 each. Indexes into `PAYMENT_METHODS` (the same
    // domain `orders::dto`/`orders::service` validate against) rather than
    // repeating the literal strings, so the two can't drift out of sync.
    const PM_CYCLE: [&str; 10] = [
        PAYMENT_METHODS[0], // credit_card
        PAYMENT_METHODS[0], // credit_card
        PAYMENT_METHODS[1], // line_pay
        PAYMENT_METHODS[0], // credit_card
        PAYMENT_METHODS[2], // atm
        PAYMENT_METHODS[0], // credit_card
        PAYMENT_METHODS[3], // jkopay
        PAYMENT_METHODS[1], // line_pay
        PAYMENT_METHODS[0], // credit_card
        PAYMENT_METHODS[4], // cash
    ];
    let mut purchases: Vec<SeedPurchase> = Vec::new();
    let mut pending_orders: Vec<PendingSeedOrder> = Vec::new();
    let mut order_total = 0usize;
    for m in (0..12u32).rev() {
        let month_first = today
            .with_day(1)
            .expect("day 1 always valid")
            .checked_sub_months(Months::new(m))
            .expect("valid seed month");
        let ym = month_first.format("%Y%m");
        let abs = month_first.year() as usize * 12 + month_first.month0() as usize;
        let count = 8 + ((abs * 7 + 3) % 5); // 8..12
        for seq in 1..=count {
            let g = abs * 12 + seq; // globally unique (count ≤ 12)
            let day = if m == 0 {
                // current month: never date an order in the future
                1 + ((seq * 3) % today.day() as usize)
            } else {
                1 + ((seq * 3 + abs * 5) % 28)
            };
            let ts = at_utc(
                month_first.with_day(day as u32).expect("day ≤ 28/today"),
                4,
                at.now,
            );
            let status = if seq == 4 {
                OrderStatus::Refunded
            } else if seq == 7 {
                OrderStatus::Pending
            } else if g.is_multiple_of(2) {
                OrderStatus::Completed
            } else {
                OrderStatus::Paid
            };

            // Lines: monthly orders sell products only — the main line
            // cycles the 7-product pool; every 3rd order appends a
            // merchandise line (unless the main line already is the tee —
            // checkout's cart can't produce two lines of one product).
            let p = g % 7;
            let mut lines: Vec<CheckoutLine> = vec![CheckoutLine {
                target: LineTarget::Product(product_ids[p]),
                quantity: 1,
                price_cents: product_seeds[p].price_cents,
                name: product_seeds[p].name.to_string(),
                is_active: true,
            }];
            if g.is_multiple_of(3) && g % 7 != 6 {
                lines.push(CheckoutLine {
                    target: LineTarget::Product(product_ids[6]),
                    quantity: 1 + (g % 2) as i32,
                    price_cents: product_seeds[6].price_cents,
                    name: product_seeds[6].name.to_string(),
                    is_active: true,
                });
            }

            let member_idx = (g * 11) % 24 + 1;
            if status == OrderStatus::Pending {
                let lines = fulfilment::ensure_all_purchasable(lines)
                    .with_context(|| format!("gate seed order DF-SEED-{ym}-{seq:02}"))?;
                pending_orders.push(PendingSeedOrder {
                    order_number: format!("DF-SEED-{ym}-{seq:02}"),
                    user_id: member_ids[member_idx - 1],
                    created_at: ts,
                    payment_method: PM_CYCLE[g % 10],
                    lines,
                });
            } else {
                // Every seq-2 order applies DREAMFLY100 — checkout loads
                // the code from the `[coupons]` row above. Seed purchases
                // never redeem points.
                purchases.push(SeedPurchase {
                    key: format!("DF-SEED-{ym}-{seq:02}"),
                    buyer: member_ids[member_idx - 1],
                    member_idx,
                    ts,
                    lines: lines.iter().map(|l| (l.target, l.quantity)).collect(),
                    coupon: (seq == 2).then_some("DREAMFLY100"),
                    payment_method: PM_CYCLE[g % 10],
                    target: status,
                });
            }
            order_total += 1;
        }
    }
    insert_pending_orders(db, &pending_orders).await?;

    // -- course purchases → enrolments (39, spread over the past 6 months) --
    // Every member buys course (i*7)%6; even i adds (i*5+1)%6; i%5==0 adds
    // (i*11+2)%6 (dedup'd) — 39 one-line purchases keyed
    // `DF-SEED-ENR-{i:02}-{k}`, each enrolling its buyer through checkout.
    // (i+k)%10==3 refunds ~10% three days later, which cancels that
    // enrolment. `ts` sits 50-179 days back, which also lands the 90-day
    // enrolment count at ~45% of the 3-month trial-inquiry count (funnel).
    let mut enrolment_total = 0usize;
    let mut enrolment_cancelled = 0usize;
    for i in 1..=24usize {
        let mut picks: Vec<usize> = vec![(i * 7) % 6];
        if i % 2 == 0 {
            let k = (i * 5 + 1) % 6;
            if !picks.contains(&k) {
                picks.push(k);
            }
        }
        if i % 5 == 0 {
            let k = (i * 11 + 2) % 6;
            if !picks.contains(&k) {
                picks.push(k);
            }
        }
        for k in picks {
            let refunded = (i + k) % 10 == 3;
            let days_ago = 50 + ((i * 13 + k * 29) % 130);
            purchases.push(SeedPurchase {
                key: format!("DF-SEED-ENR-{i:02}-{k}"),
                buyer: member_ids[i - 1],
                member_idx: i,
                ts: at_utc(today - Days::new(days_ago as u64), 6, at.now),
                lines: vec![(LineTarget::Course(course_ids[k]), 1)],
                coupon: None,
                payment_method: PM_CYCLE[(i + k) % 10],
                target: if refunded {
                    OrderStatus::Refunded
                } else {
                    OrderStatus::Paid
                },
            });
            enrolment_total += 1;
            if refunded {
                enrolment_cancelled += 1;
            }
        }
    }

    // Both sets go through the seed purchase timeline (`timeline`), which
    // buys and walks them in time order. The active enrolments attendance
    // is marked against are the ones each order holds once walked, in the
    // order bought.
    let walked = timeline::buy_and_walk(db, at, purchases).await?;
    let mut active_enrolments: Vec<(usize, usize, Uuid)> = Vec::new();
    for Walked { member_idx, order } in walked {
        for enrolment in order.enrolments {
            if enrolment.status == EnrolmentStatus::Active {
                let k = course_ids
                    .iter()
                    .position(|&id| id == enrolment.course_id)
                    .expect("seed purchases only buy seed courses");
                active_enrolments.push((member_idx, k, enrolment.id));
            }
        }
    }
    println!(
        "[orders]   {order_total} monthly + {enrolment_total} course seed orders ready \
         (checkout key DF-SEED-*, pending order_number DF-SEED-*)"
    );
    println!(
        "[enrolments] {} enrolments ready ({} active / {} cancelled)",
        enrolment_total,
        active_enrolments.len(),
        enrolment_cancelled
    );

    // -- settle every member's points-tier target balance --------------------
    // After every purchase above has written its ledger rows (see
    // `settlement`).
    let settled = settle_members(db, &member_ids, at.now).await?;
    println!("[points]   {settled} members settled to their points-tier target");

    // -- course sessions: materialize the past 6 months ---------------------
    let six_months_ago = today
        .checked_sub_months(Months::new(6))
        .expect("valid seed range");
    backfill_for_seed(db, &course_ids, six_months_ago, today)
        .await
        .context("materialize course sessions for the past 6 months")?;
    let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions")
        .fetch_one(db)
        .await?;
    println!("[sessions] materialized {six_months_ago} → {today} ({session_count} sessions total)");

    // -- attendance (~1500-2000 rows) ---------------------------------------
    // Every active enrolment × that course's already-materialized past
    // sessions; the status formula and `marked_at` are `attendance`'s.
    let marked =
        attendance::mark_past_sessions(db, at, admin_id, &course_ids, &active_enrolments).await?;
    println!("[attendance] {marked} attendance records ready");

    // -- venue rental: time_slots (~168) + bookings --------------------------
    // The rental calendar and its past bookings are `rental::seed_rentals`'s
    // (rules in its doc); `run` only fixes the 4 venues and their prices.
    let venues = [
        (venue_id_by_slug(db, "trampoline-zone").await?, 80_000),
        (
            venue_id_by_slug(db, "floor-gymnastics-zone").await?,
            100_000,
        ),
        (venue_id_by_slug(db, "aerial-skills-zone").await?, 130_000),
        (venue_id_by_slug(db, "kids-play-zone").await?, 150_000),
    ];
    let (rental_slots, rental_bookings) = seed_rentals(db, at, &venues, &member_ids).await?;
    println!("[rental]   {rental_slots} rental time slots / {rental_bookings} bookings ready");

    // -- contact inquiries (past 3 months) -----------------------------------
    // Month k (0 = current .. 2): 8-10 trial + 3-4 general per month.
    // Idempotency key: the `df-seed-…` email. Trial metadata mirrors the
    // mobile 試上 flow's structured fields.
    const CATEGORIES: [&str; 4] = ["體操", "啦啦", "跑酷", "幼兒"];
    const WEEKDAYS: [&str; 7] = ["週一", "週二", "週三", "週四", "週五", "週六", "週日"];
    const TRIAL_SLOTS: [&str; 3] = ["10:00-11:00", "16:00-17:00", "19:00-20:00"];
    let mut inquiry_trials = 0usize;
    let mut inquiry_generals = 0usize;
    for k in 0..3u32 {
        let month_first = today
            .with_day(1)
            .expect("day 1 always valid")
            .checked_sub_months(Months::new(k))
            .expect("valid seed month");
        let ym = month_first.format("%Y%m");
        let clamp = |raw: usize| -> u32 {
            if k == 0 {
                1 + (raw % today.day() as usize) as u32
            } else {
                1 + (raw % 27) as u32
            }
        };

        let trial_count = 8 + ((k as usize * 7 + 1) % 3); // 9 / 10 / 8
        for seq in 1..=trial_count {
            let category = CATEGORIES[(seq + k as usize) % 4];
            let parent_name = format!("試上家長{seq:02}");
            let phone = format!("09{:08}", (k as usize * 37 + seq * 13) % 100_000_000);
            let created_at = at_utc(
                month_first
                    .with_day(clamp(seq * 5 + k as usize * 3))
                    .expect("valid day"),
                2,
                at.now,
            );
            insert_inquiry_if_absent(
                db,
                &InquirySeed {
                    email: format!("df-seed-trial-{ym}-{seq:02}@example.com"),
                    name: parent_name.clone(),
                    phone: phone.clone(),
                    subject: format!("【試上申請】{category} 課程試上"),
                    message: "想為孩子預約一堂試上課程，請與我聯繫安排時間。",
                    inquiry_type: InquiryType::Trial.as_str(),
                    metadata: Some(json!({
                        "category": category,
                        "student_age": 5 + ((seq * 3 + k as usize) % 10),
                        "preferred_day": WEEKDAYS[(seq + k as usize) % 7],
                        "preferred_slot": TRIAL_SLOTS[seq % 3],
                        "parent_name": parent_name,
                        "parent_phone": phone,
                        "student_name": format!("試上學員{seq:02}"),
                        "note": "希望先參觀場地再決定",
                    })),
                    created_at,
                },
            )
            .await?;
            inquiry_trials += 1;
        }

        let general_count = 3 + (k as usize % 2); // 3 / 4 / 3
        for seq in 1..=general_count {
            const SUBJECTS: [&str; 4] = [
                "課程費用詢問",
                "場地租借詢問",
                "會員方案詢問",
                "營業時間詢問",
            ];
            let created_at = at_utc(
                month_first
                    .with_day(clamp(seq * 7 + k as usize * 5))
                    .expect("valid day"),
                2,
                at.now,
            );
            insert_inquiry_if_absent(
                db,
                &InquirySeed {
                    email: format!("df-seed-general-{ym}-{seq:02}@example.com"),
                    name: format!("洽詢民眾{seq:02}"),
                    phone: format!("09{:08}", (k as usize * 53 + seq * 17) % 100_000_000),
                    subject: SUBJECTS[(seq + k as usize) % 4].to_string(),
                    message: "您好，想進一步了解相關資訊，再麻煩回覆，謝謝。",
                    inquiry_type: InquiryType::General.as_str(),
                    metadata: None,
                    created_at,
                },
            )
            .await?;
            inquiry_generals += 1;
        }
    }
    println!(
        "[contact]  {} inquiries ready ({inquiry_trials} trial / {inquiry_generals} general)",
        inquiry_trials + inquiry_generals
    );

    let row_counts = collect_row_counts(db).await?;
    Ok(SeedReport {
        row_counts,
        settled_members: settled,
    })
}
