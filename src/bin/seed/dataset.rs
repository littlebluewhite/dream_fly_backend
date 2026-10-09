//! The dev/reporting dataset `dataset::run` writes — split out of
//! `bin/seed/main.rs` (the entry point: env/config/pool/migrate, then one
//! sampled `StudioNow` handed in here) so the dataset itself, `SeedReport`
//! and its `#[sqlx::test]`s can live together without dragging in
//! process-level concerns.
//!
//! Every insert is `INSERT ... ON CONFLICT DO NOTHING` keyed on the table's
//! natural unique column (email / slug / code) — a bare `ON CONFLICT DO
//! NOTHING` (no explicit target) no-ops on ANY unique-constraint violation on
//! that table, so this is safe regardless of whether the unique constraint is
//! a plain column or a functional `LOWER(slug)` index. Where a table has no
//! usable unique key (rewards, enrolments, orders, time_slots, bookings,
//! contact_inquiries), idempotency is a SELECT-existence check on the seed's
//! deterministic natural key (name / user+course / `DF-SEED-…` order number /
//! venue+date+start / user+slot / `df-seed-…` email) instead. Running this
//! binary twice must therefore leave row counts unchanged on the second run.
//!
//! The 12-month reporting dataset (members / enrolments / orders / sessions /
//! attendance / venue-rental slots+bookings / inquiries) is **deterministic**:
//! every value derives from fixed index formulas over the run date — no
//! randomness — so re-running on the same day produces the identical set.
//!
//! `points_balance` is granted through `points::service::apply_delta_tx`,
//! never written to the column directly, so `points_balance ==
//! SUM(point_ledger.delta)` holds for every seeded user — but only on a
//! freshly migrated database: rows left over from an older run of this seed
//! (which wrote the column directly, with no backing ledger) are not
//! backfilled, an accepted legacy shape. Orders created by an older version
//! of this seed binary (before it wrote `checkout_earn`/`refund_clawback`
//! rows, or while it still stamped them with the run instant instead of the
//! order's `paid_at`) similarly keep their old ledger shape on a re-run —
//! idempotency is keyed on `order_number`, so existing rows are never
//! rewritten; reset the dev database to pick up the new shape from scratch.
//! Named accounts (`upsert_user`) get their whole balance via one
//! `LedgerDelta::admin_adjust` in the same transaction as the INSERT. The 24
//! reporting members (`upsert_seed_member`) instead earn theirs the way a
//! real member would: `insert_order_if_absent` writes a `checkout_earn` row
//! (and, for refunded orders, a `refund_clawback` row on top) for every
//! seeded order touching that member, in the same transaction as the order
//! — amounts priced by `orders::pricing::price`, rows written by checkout's
//! own `orders::paid_order::record_paid_order_tx`
//! (`PricingOutcome::ledger_deltas`) and, for refunds,
//! `points::service::reverse_order_tx`, all stamped with the order's
//! `paid_at` rather than the run instant;
//! once every order is inserted, **every** member (not just ones newly
//! created this run) gets a `admin_adjust(target − balance)` settling it to
//! its points-tier target (`member_points_target`, derived from
//! `PointsTier`), every run — new-month orders keep adding
//! `checkout_earn` for existing members too (the 12-month window slides),
//! so without an every-run settlement their balance would drift past its
//! tier boundary over time. A same-month re-run computes `delta = 0` and
//! writes nothing, so this stays idempotent; a run that crashes between the
//! order loop and the settlement loop simply gets settled by the next run
//! instead (settlement isn't gated on "this member is new"). One side
//! effect: this also means a developer's manual `UPDATE users SET
//! points_balance = ...` against a seed-member row is reset back to its
//! tier target the next time the seed runs.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Datelike, Days, Duration, Months, NaiveDate, NaiveTime, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::cart::model::{CheckoutLine, LineTarget};
use dream_fly_backend::modules::contact::model::InquiryType;
use dream_fly_backend::modules::coupons::repository as coupons_repository;
use dream_fly_backend::modules::orders::fulfilment::{self, PurchasableCart};
use dream_fly_backend::modules::orders::model::{OrderStatus, PAYMENT_METHODS};
use dream_fly_backend::modules::orders::paid_order::record_paid_order_tx;
use dream_fly_backend::modules::orders::pricing::{self, PricingOutcome};
use dream_fly_backend::modules::orders::repository as orders_repository;
use dream_fly_backend::modules::permissions::model::Role;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::modules::points::model::{LedgerDelta, PointsTier};
use dream_fly_backend::modules::points::service as points_service;
use dream_fly_backend::modules::sessions::calendar::backfill_for_seed;
use dream_fly_backend::utils::password;
use dream_fly_backend::utils::studio_clock::StudioNow;

/// Convert a fixed list of `&str` literals into the `Vec<String>` sqlx needs
/// to bind a Postgres `TEXT[]` column.
fn vs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// users + roles
// ---------------------------------------------------------------------------

/// Insert a user (idempotent on `email`) and return its id whether the row
/// was just inserted or already existed. `created_at` is the account's
/// historical join instant (`run`'s `joined_at`), not the run instant — the
/// user must predate the seeded rows that reference it. Takes an already-hashed password
/// (see `run`'s one-Argon2-per-plaintext precompute of the Admin/Member/
/// Coach hashes) rather than hashing here, so callers sharing a plaintext
/// share the hash instead of re-hashing it per call. The INSERT always
/// writes `points_balance = 0`; when the row is newly inserted (not a
/// conflict) and `points_balance > 0`, the target balance is granted via
/// `points_service::apply_delta_tx` (`LedgerDelta::admin_adjust`) in the same
/// transaction as the INSERT — user row and `point_ledger` grant land
/// atomically (a failed grant rolls back the insert too), and a conflict
/// (already exists) never grants, so re-runs stay idempotent.
async fn upsert_user(
    db: &PgPool,
    email: &str,
    name: &str,
    password_hash: &str,
    points_balance: i64,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let mut tx = db
        .begin()
        .await
        .with_context(|| format!("begin tx for user {email}"))?;

    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, points_balance, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, 0, $5, NOW())
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(password_hash)
    .bind(created_at)
    .fetch_optional(&mut *tx)
    .await
    .with_context(|| format!("insert user {email}"))?;

    let user_id = match inserted {
        Some(id) => {
            if points_balance > 0 {
                points_service::apply_delta_tx(
                    &mut tx,
                    id,
                    LedgerDelta::admin_adjust(points_balance),
                    now,
                )
                .await
                .with_context(|| format!("grant seed points to user {email}"))?;
            }
            id
        }
        None => sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(&mut *tx)
            .await
            .with_context(|| format!("fetch id for user {email}"))?,
    };

    tx.commit()
        .await
        .with_context(|| format!("commit tx for user {email}"))?;

    Ok(user_id)
}

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
    date.and_hms_opt(hour, 0, 0).expect("valid seed hour").and_utc().min(now)
}

/// Insert a reporting-dataset member (idempotent on `email`) and return its
/// id. Unlike `upsert_user` this takes a precomputed hash (all 24 members
/// share one dev password — hashing argon2 once instead of 24× per run) and
/// writes `birth_date`, which backs the age-bracket report bucket. Unlike
/// `upsert_user`, this function grants no points itself: the points-tier
/// target balance is instead reached through the member's seeded orders'
/// `checkout_earn`/`refund_clawback` rows (written by
/// `insert_order_if_absent`) plus a settling `LedgerDelta::admin_adjust` the
/// caller applies, every run, once every order is inserted (see the module
/// doc) — so this function doesn't need to report whether the row was newly
/// inserted; the settlement step runs for every member regardless.
/// `created_at` is the member's historical join instant, as in `upsert_user`.
async fn upsert_seed_member(
    db: &PgPool,
    email: &str,
    name: &str,
    password_hash: &str,
    birth_date: NaiveDate,
    created_at: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let mut tx = db
        .begin()
        .await
        .with_context(|| format!("begin tx for seed member {email}"))?;

    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, points_balance, birth_date, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, 0, $5, $6, NOW())
        ON CONFLICT DO NOTHING
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(password_hash)
    .bind(birth_date)
    .bind(created_at)
    .fetch_optional(&mut *tx)
    .await
    .with_context(|| format!("insert seed member {email}"))?;

    let user_id = match inserted {
        Some(id) => id,
        None => sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(&mut *tx)
            .await
            .with_context(|| format!("fetch id for seed member {email}"))?,
    };

    tx.commit()
        .await
        .with_context(|| format!("commit tx for seed member {email}"))?;

    Ok(user_id)
}

/// Points-tier target balance for reporting member `i` (1..=24): members
/// block into `PointsTier::ALL` six at a time on `(i-1)/6`, and each lands
/// a fixed step above its tier's `floor()` — so the report's 4-tier /
/// 6-members-each shape follows the tier owner rather than a second copy
/// of its boundaries.
fn member_points_target(i: usize) -> i64 {
    let tier = PointsTier::ALL[(i - 1) / 6];
    let j = ((i - 1) % 6 + 1) as i64; // 1..=6 within the tier
    let above_floor = match tier {
        PointsTier::Regular => 100 + j * 50, //  150-400
        PointsTier::Bronze => j * 200,       //  700-1700
        PointsTier::Silver => j * 400,       // 2400-4400
        PointsTier::Gold => j * 500,         // 5500-8000
    };
    tier.floor() + above_floor
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

/// Insert an enrolment (idempotent by existence check on `(user_id,
/// course_id)` — the table's only unique index is partial on `status =
/// 'active'`, so cancelled rows have no ON CONFLICT target) and return the
/// row's id whether just inserted or pre-existing. The seed assigns at most
/// one enrolment per (member, course) pair, so any-status existence is the
/// right key.
async fn insert_enrolment_if_absent(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    status: &str,
    created_at: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM enrolments WHERE user_id = $1 AND course_id = $2 LIMIT 1",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_optional(db)
    .await
    .context("check existing enrolment")?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO enrolments (id, user_id, course_id, order_id, status, enrolled_at, created_at, updated_at)
        VALUES ($1, $2, $3, NULL, $4::enrolment_status, $5, $5, $5)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(course_id)
    .bind(status)
    .bind(created_at)
    .execute(db)
    .await
    .with_context(|| format!("insert enrolment {user_id}/{course_id}"))?;
    Ok(id)
}

/// A checkout-shaped order. `lines` is the same `PurchasableCart` checkout
/// prices (built through `fulfilment::ensure_all_purchasable`, its only
/// constructor), and `pricing` is `orders::pricing::price`'s outcome for them
/// (the optional coupon loaded via `coupons::repository::find_valid_by_code`,
/// never redeeming points) — so amounts, discount, applied code and
/// `points_earned` come from the pricing owner, not a seed-side copy of its
/// arithmetic. `insert_order_if_absent` maps `status` exhaustively: the
/// paid family (and refunded, which keeps its original `paid_at`, as the
/// refund path never clears it) goes through
/// `orders::paid_order::record_paid_order_tx` at `created_at` — order row
/// (`paid_at = created_at`), order items and `PricingOutcome::ledger_deltas`
/// (a `checkout_earn` row) all stamped with that one instant; refunded
/// additionally runs `points::service::reverse_order_tx` at the same instant
/// (a `refund_clawback` of the same magnitude — net zero); pending contrast
/// rows record `paid_at = NULL`, `points_earned = 0` and no ledger rows.
/// Both delta owners skip zero magnitudes, so a fully-discounted order
/// writes none.
struct SeedOrder {
    order_number: String,
    user_id: Uuid,
    status: OrderStatus,
    created_at: DateTime<Utc>,
    payment_method: &'static str,
    lines: PurchasableCart,
    pricing: PricingOutcome,
}

/// Insert an order + its order_items (idempotent by existence check on
/// `order_number` — order_items has no unique key of its own, so the parent
/// check guards both). Not the checkout service (no cart dependency), but
/// the paid family is written by the same paid-order record checkout uses,
/// under the buyer's `BalanceLock`; a status other than paid is then set
/// with a direct `update_status_tx` — not the order state machine, this is
/// dev data. Pending/cancelled keep a direct `orders` INSERT
/// (`record_paid_order_tx` only writes `status = 'paid'`) and share
/// `create_order_items`. The order's `point_ledger` rows (see `SeedOrder`'s
/// doc) land in the same transaction, guarded by the same up-front
/// existence check, so a re-run neither duplicates the order nor its ledger
/// rows.
async fn insert_order_if_absent(db: &PgPool, seed: &SeedOrder) -> anyhow::Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orders WHERE order_number = $1)")
            .bind(&seed.order_number)
            .fetch_one(db)
            .await
            .with_context(|| format!("check existing order '{}'", seed.order_number))?;
    if exists {
        return Ok(());
    }

    let mut tx = db.begin().await.context("begin order tx")?;
    // Seed never decrements stock, so no line is marked `stock_decremented`.
    let lines = fulfilment::order_lines(&seed.lines, &HashMap::new());

    // Exhaustive over `OrderStatus` — a new variant must decide here
    // whether it was ever paid (and so goes through the paid-order record
    // and the ledger). The seed itself only produces pending / paid /
    // completed / refunded.
    match seed.status {
        // Never paid (`Cancelled` read as the `Pending → Cancelled` edge):
        // no `paid_at`, nothing earned or redeemed, no ledger rows.
        OrderStatus::Pending | OrderStatus::Cancelled => {
            let order_id = Uuid::now_v7();
            sqlx::query(
                r#"
                INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents,
                                    coupon_code, points_used, points_earned, payment_method, paid_at,
                                    created_at, updated_at)
                VALUES ($1, $2, $3, $4::order_status, $5, $6, $7, 0, 0, $8, NULL, $9, $9)
                "#,
            )
            .bind(order_id)
            .bind(seed.user_id)
            .bind(&seed.order_number)
            .bind(seed.status.as_str())
            .bind(seed.pricing.total_cents)
            .bind(seed.pricing.discount_cents)
            .bind(&seed.pricing.applied_coupon_code)
            .bind(seed.payment_method)
            .bind(seed.created_at)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("insert order '{}'", seed.order_number))?;

            orders_repository::create_order_items(&mut tx, order_id, &lines, seed.created_at)
                .await
                .with_context(|| format!("insert order_items for '{}'", seed.order_number))?;
        }
        // Recorded paid at `created_at` first. Refunded keeps that original
        // `paid_at` (the refund path never clears it) and its checkout
        // ledger rows, then reverses that flow on top. Seed prices with
        // `use_points = false`, so there is never a redeem for the locked
        // balance to cover.
        OrderStatus::Paid
        | OrderStatus::Processing
        | OrderStatus::Completed
        | OrderStatus::Refunded => {
            let lock = points_service::lock_balance_tx(&mut tx, seed.user_id)
                .await
                .with_context(|| format!("lock balance for order '{}'", seed.order_number))?;
            let order = record_paid_order_tx(
                &mut tx,
                &lock,
                &seed.order_number,
                seed.payment_method,
                &lines,
                &seed.pricing,
                seed.created_at,
            )
            .await
            .with_context(|| format!("record paid order '{}'", seed.order_number))?;

            if seed.status != OrderStatus::Paid {
                orders_repository::update_status_tx(&mut tx, order.id, &seed.status)
                    .await
                    .with_context(|| format!("set status of order '{}'", seed.order_number))?;
            }
            if seed.status == OrderStatus::Refunded {
                points_service::reverse_order_tx(&mut tx, &lock, order.id, seed.created_at)
                    .await
                    .with_context(|| format!("refund ledger for order '{}'", seed.order_number))?;
            }
        }
    }

    tx.commit().await.context("commit order tx")?;
    Ok(())
}

/// Bulk-insert attendance rows (idempotent on the real
/// `UNIQUE(session_id, enrolment_id)` constraint). Single UNNEST statement,
/// mirroring the UNNEST bulk INSERT of `sessions::calendar`'s private
/// slot-date insert (behind `backfill_for_seed`).
async fn insert_attendance_bulk(
    db: &PgPool,
    marked_by: Uuid,
    rows: &[(Uuid, Uuid, &'static str, DateTime<Utc>)],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut session_ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut enrolment_ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut statuses: Vec<String> = Vec::with_capacity(rows.len());
    let mut marked_ats: Vec<DateTime<Utc>> = Vec::with_capacity(rows.len());
    for (session_id, enrolment_id, status, marked_at) in rows {
        ids.push(Uuid::now_v7());
        session_ids.push(*session_id);
        enrolment_ids.push(*enrolment_id);
        statuses.push((*status).to_string());
        marked_ats.push(*marked_at);
    }

    sqlx::query(
        "INSERT INTO attendance_records (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         SELECT u.id, u.session_id, u.enrolment_id, u.status::attendance_status, $2, u.marked_at, NOW() \
         FROM UNNEST($1::uuid[], $3::uuid[], $4::uuid[], $5::text[], $6::timestamptz[]) \
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

/// One venue-rental time slot, column shape per
/// `schedule::repository::bulk_create_tx` (course_id NULL — pure rental).
struct TimeSlotSeed {
    venue_id: Uuid,
    date: NaiveDate,
    start_time: NaiveTime,
    end_time: NaiveTime,
    capacity: i32,
    price_cents: i64,
}

/// Insert a rental slot (idempotent by existence check on
/// `(venue_id, date, start_time)` — the table's only guard is the GIST
/// anti-overlap EXCLUDE, which bare ON CONFLICT DO NOTHING also covers as a
/// belt) and return its id either way. The slot stores no occupancy count:
/// its `booked` is counted at read time from the `occupying_bookings` view
/// (ADR-0015), so a slot that crosses into the past and gains a booking on a
/// later run needs no sync.
async fn upsert_time_slot(db: &PgPool, seed: &TimeSlotSeed) -> anyhow::Result<Uuid> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM time_slots WHERE venue_id = $1 AND date = $2 AND start_time = $3",
    )
    .bind(seed.venue_id)
    .bind(seed.date)
    .bind(seed.start_time)
    .fetch_optional(db)
    .await
    .context("check existing time_slot")?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(id)
    .bind(seed.date)
    .bind(seed.start_time)
    .bind(seed.end_time)
    .bind(seed.venue_id)
    .bind(seed.capacity)
    .bind(seed.price_cents)
    .execute(db)
    .await
    .with_context(|| format!("insert time_slot {} {}", seed.date, seed.start_time))?;
    Ok(id)
}

/// Insert a rental booking (idempotent by existence check on
/// `(user_id, time_slot_id)` — the partial unique index excludes cancelled
/// rows, and the seed assigns at most one booking per slot). `price_cents`
/// is the slot's price snapshot, per
/// `bookings::occupancy::insert_occupying_booking_tx`.
async fn insert_booking_if_absent(
    db: &PgPool,
    user_id: Uuid,
    time_slot_id: Uuid,
    status: &str,
    price_cents: i64,
    created_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM bookings WHERE user_id = $1 AND time_slot_id = $2)",
    )
    .bind(user_id)
    .bind(time_slot_id)
    .fetch_one(db)
    .await
    .context("check existing booking")?;
    if exists {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO bookings (id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at)
        VALUES ($1, $2, $3, $4::booking_status, NULL, $5, $6, $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(time_slot_id)
    .bind(status)
    .bind(price_cents)
    .bind(created_at)
    .execute(db)
    .await
    .with_context(|| format!("insert booking for slot {time_slot_id}"))?;
    Ok(())
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
    const QUERIES: [(&str, &str); 16] = [
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
        ("enrolments", "SELECT COUNT(*) FROM enrolments"),
        ("attendance_records", "SELECT COUNT(*) FROM attendance_records"),
        ("course_sessions", "SELECT COUNT(*) FROM course_sessions"),
        ("time_slots", "SELECT COUNT(*) FROM time_slots"),
        ("bookings", "SELECT COUNT(*) FROM bookings"),
        ("contact_inquiries", "SELECT COUNT(*) FROM contact_inquiries"),
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

/// The seed dataset itself — everything `bin/seed/main.rs`'s entry point
/// (env/config/pool/migrate, one sampled `at: StudioNow`) hands off to.
/// Idempotent and safe to re-run: see the module doc for the per-table
/// idempotency keys and the points-ledger settlement shape.
pub async fn run(db: &PgPool, at: StudioNow) -> anyhow::Result<SeedReport> {
    // Hash each distinct dev plaintext once, up front — Admin#2026 (admin),
    // Member#2026 (test member + all 24 reporting members) and Coach#2026
    // (all 4 coaches) — instead of once per `upsert_user`/`upsert_seed_member`
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
    let admin_id = upsert_user(
        db,
        "admin@dreamfly.tw",
        "系統管理員",
        &admin_hash,
        0,
        joined_at,
        at.now,
    )
    .await?;
    assign_role(db, admin_id, Role::Admin).await?;
    println!("[users]    admin ready: admin@dreamfly.tw / Admin#2026");

    // -- test member -------------------------------------------------------
    let member_id = upsert_user(
        db,
        "member@dreamfly.tw",
        "測試會員",
        &member_hash,
        1250,
        joined_at,
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
        let user_id = upsert_user(
            db,
            seed.email,
            seed.user_name,
            &coach_hash,
            0,
            joined_at,
            at.now,
        )
        .await?;
        assign_role(db, user_id, Role::Coach).await?;
        let coach_id = upsert_coach(db, user_id, seed).await?;
        coach_ids.insert(seed.slug, coach_id);
        println!("[coaches]  {} ready: {} / Coach#2026", seed.user_name, seed.email);
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
            insert_course_schedule_slot(db, course_id, *day_of_week, start_time, end_time, seed.venue)
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
            features: &["30 天不限次數上課", "可預約所有常規課程", "隨時開始，效期 30 天"],
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
            features: &["90 天不限次數上課", "比月票更優惠的平均月費", "適合中長期訓練規劃"],
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
            features: &["365 天不限次數上課", "全年最優惠平均月費", "專屬會員生日禮遇"],
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
    // `member_points_target`. Different index bases so age and tier
    // decorrelate. `member_hash` is the same Member#2026 hash the test
    // member above shares — hashed once, up front (see `run`'s precompute).
    let mut member_ids: Vec<Uuid> = Vec::with_capacity(24);
    // `(user_id, points-tier target balance)` for every one of the 24
    // members — settled to its tier target after the order loop below has
    // written every seeded order's `checkout_earn`/`refund_clawback` rows
    // (see the settlement loop after `[orders]`). Collected for every
    // member, not just newly-inserted ones: a settling member's balance
    // must be re-pinned to `target` on every run, not just its first, so
    // that new-month orders (this run's `[orders]` loop keeps writing
    // `checkout_earn` for existing members too) can't push an existing
    // member's balance past its tier boundary over time.
    let mut member_targets: Vec<(Uuid, i64)> = Vec::with_capacity(24);
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
        let user_id =
            upsert_seed_member(db, &email, &name, &member_hash, birth_date, joined_at).await?;
        assign_role(db, user_id, Role::Member).await?;
        member_targets.push((user_id, member_points_target(i)));
        member_ids.push(user_id);
    }
    println!("[members]  24 seed members ready (seed-member-01..24@dreamfly.tw / Member#2026)");

    // -- enrolments (~39, spread over the past 6 months) --------------------
    // Every member takes course (i*7)%6; even i adds (i*5+1)%6; i%5==0 adds
    // (i*11+2)%6 (dedup'd) — 39 pairs. (i+k)%10==3 marks ~10% cancelled.
    // created_at sits 50-179 days back, which also lands the 90-day
    // enrolment count at ~45% of the 3-month trial-inquiry count (funnel).
    let mut active_enrolments: Vec<(usize, usize, Uuid)> = Vec::new();
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
            let status = if (i + k) % 10 == 3 { "cancelled" } else { "active" };
            let days_ago = 50 + ((i * 13 + k * 29) % 130);
            let created_at = at_utc(today - Days::new(days_ago as u64), 6, at.now);
            let enrolment_id =
                insert_enrolment_if_absent(db, member_ids[i - 1], course_ids[k], status, created_at)
                    .await?;
            enrolment_total += 1;
            if status == "active" {
                active_enrolments.push((i, k, enrolment_id));
            } else {
                enrolment_cancelled += 1;
            }
        }
    }
    println!(
        "[enrolments] {} enrolments ready ({} active / {} cancelled)",
        enrolment_total,
        active_enrolments.len(),
        enrolment_cancelled
    );

    // -- orders + order_items (~119 across 12 months) -----------------------
    // Idempotency key: order_number `DF-SEED-{YYYYMM}-{seq:02}`. Month m
    // (0 = current .. 11 = oldest) gets 8 + ((m*7+3)%5) orders; every month
    // carries one refunded (seq 4) and one pending (seq 7) contrast row.
    // Months run oldest first, so each member's ledger `balance_after`
    // moves forward with the rows' `paid_at`; nothing below depends on the
    // visiting order (`g`, member and status derive from `(m, seq)` alone).
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
    // Every seq-2 order applies DREAMFLY100 — loaded once, the way checkout
    // loads a code, so its discount comes from the `[coupons]` row above.
    let seed_coupon = coupons_repository::find_valid_by_code(db, "DREAMFLY100", at.now)
        .await
        .context("load seed coupon DREAMFLY100")?
        .context("seed coupon DREAMFLY100 is missing, inactive or expired")?;
    let mut order_total = 0usize;
    for m in (0..12u32).rev() {
        let month_first = today
            .with_day(1)
            .expect("day 1 always valid")
            .checked_sub_months(Months::new(m))
            .expect("valid seed month");
        let ym = month_first.format("%Y%m");
        let count = 8 + ((m as usize * 7 + 3) % 5); // 8..12
        for seq in 1..=count {
            let g = m as usize * 12 + seq; // globally unique (count ≤ 12)
            let day = if m == 0 {
                // current month: never date an order in the future
                1 + ((seq * 3) % today.day() as usize)
            } else {
                1 + ((seq * 3 + m as usize * 5) % 28)
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

            // Lines: every 5th order is a course enrolment purchase, the
            // rest cycle the 7-product pool; every 3rd order appends a
            // merchandise line (unless the main line already is the tee —
            // checkout's cart can't produce two lines of one product).
            let mut lines: Vec<CheckoutLine> = Vec::new();
            if g.is_multiple_of(5) {
                let k = (g / 5) % 6;
                lines.push(CheckoutLine {
                    target: LineTarget::Course(course_ids[k]),
                    quantity: 1,
                    price_cents: course_seeds[k].price_cents,
                    name: course_seeds[k].name.to_string(),
                    is_active: true,
                });
            } else {
                let p = g % 7;
                lines.push(CheckoutLine {
                    target: LineTarget::Product(product_ids[p]),
                    quantity: 1,
                    price_cents: product_seeds[p].price_cents,
                    name: product_seeds[p].name.to_string(),
                    is_active: true,
                });
            }
            if g.is_multiple_of(3) && (g.is_multiple_of(5) || g % 7 != 6) {
                lines.push(CheckoutLine {
                    target: LineTarget::Product(product_ids[6]),
                    quantity: 1 + (g % 2) as i32,
                    price_cents: product_seeds[6].price_cents,
                    name: product_seeds[6].name.to_string(),
                    is_active: true,
                });
            }

            // Amounts and points through checkout's own pricing owner —
            // seed orders never redeem points.
            let coupon = if seq == 2 { Some(&seed_coupon) } else { None };
            let lines = fulfilment::ensure_all_purchasable(lines)
                .with_context(|| format!("gate seed order DF-SEED-{ym}-{seq:02}"))?;
            let pricing = pricing::price(&lines, coupon, 0, false)
                .with_context(|| format!("price seed order DF-SEED-{ym}-{seq:02}"))?;

            insert_order_if_absent(
                db,
                &SeedOrder {
                    order_number: format!("DF-SEED-{ym}-{seq:02}"),
                    user_id: member_ids[(g * 11) % 24],
                    status,
                    created_at: ts,
                    payment_method: PM_CYCLE[g % 10],
                    lines,
                    pricing,
                },
            )
            .await?;
            order_total += 1;
        }
    }
    println!("[orders]   {order_total} seed orders across 12 months ready (DF-SEED-*)");

    // -- settle every member's points-tier target balance --------------------
    // Every run, for every one of the 24 members (not just newly-inserted
    // ones): the member's balance so far is whatever its seeded orders'
    // `checkout_earn`/`refund_clawback` rows above added up to (a refunded
    // order nets zero); `admin_adjust(target − balance)` tops (or trims)
    // that up to the exact points-tier target computed in the members loop,
    // so `points_balance == SUM(point_ledger.delta)` still holds while
    // keeping the 4-tier/6-members-each report shape. A same-month re-run
    // adds no new orders for a member already at target, so `delta` is 0
    // and no row is written — this is what keeps the seed idempotent
    // despite settling unconditionally. A crash between the order loop and
    // this loop simply means the next run's settlement (unconditional, not
    // gated on "newly inserted") finishes the job. This also means any
    // manual `UPDATE users SET points_balance = ...` a developer runs by
    // hand against a seed-member row is reset back to its tier target on
    // the next `cargo run --bin seed`.
    let mut settled = 0usize;
    for (user_id, target) in &member_targets {
        let balance: i64 = sqlx::query_scalar("SELECT points_balance FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(db)
            .await
            .with_context(|| format!("fetch points_balance for seed member {user_id}"))?;
        let delta = target - balance;
        if delta != 0 {
            let mut tx = db
                .begin()
                .await
                .with_context(|| format!("begin points settlement tx for seed member {user_id}"))?;
            points_service::apply_delta_tx(
                &mut tx,
                *user_id,
                LedgerDelta::admin_adjust(delta),
                at.now,
            )
            .await
            .with_context(|| format!("settle points-tier balance for seed member {user_id}"))?;
            tx.commit()
                .await
                .with_context(|| format!("commit points settlement for seed member {user_id}"))?;
            settled += 1;
        }
    }
    println!("[points]   {settled} members settled to their points-tier target");

    // -- course sessions: materialize the past 6 months ---------------------
    let six_months_ago = today.checked_sub_months(Months::new(6)).expect("valid seed range");
    backfill_for_seed(db, &course_ids, six_months_ago, today)
        .await
        .context("materialize course sessions for the past 6 months")?;
    let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions")
        .fetch_one(db)
        .await?;
    println!("[sessions] materialized {six_months_ago} → {today} ({session_count} sessions total)");

    // -- attendance (~1500-2000 rows) ---------------------------------------
    // Every active enrolment × that course's already-materialized past
    // sessions. `(member_idx + session_idx) % 20` → 0-14 present / 15-17
    // absent / 18-19 leave (~75/15/10); members 22-24 are the low-attendance
    // group → 0-7 present / 8-16 absent / 17-19 leave (~40/45/15), filling
    // the attendance-distribution report's low buckets.
    // `marked_at` treats the session's studio-local `end_time` as if it
    // were already UTC (same shortcut `at_utc` takes), then clamps to
    // `at.now` — same clamp reason as `at_utc`: a session dated `today`
    // (studio-local, can run up to a day ahead of the UTC date) would
    // otherwise mark attendance hours in the future.
    let mut past_sessions: Vec<Vec<(Uuid, DateTime<Utc>)>> = Vec::with_capacity(course_ids.len());
    for course_id in &course_ids {
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
    let mut attendance_rows: Vec<(Uuid, Uuid, &'static str, DateTime<Utc>)> = Vec::new();
    for (i, k, enrolment_id) in &active_enrolments {
        for (session_idx, (session_id, marked_at)) in past_sessions[*k].iter().enumerate() {
            let r = (i + session_idx) % 20;
            let status = if *i >= 22 {
                match r {
                    0..=7 => "present",
                    8..=16 => "absent",
                    _ => "leave",
                }
            } else {
                match r {
                    0..=14 => "present",
                    15..=17 => "absent",
                    _ => "leave",
                }
            };
            attendance_rows.push((*session_id, *enrolment_id, status, *marked_at));
        }
    }
    insert_attendance_bulk(db, admin_id, &attendance_rows).await?;
    println!("[attendance] {} attendance records ready", attendance_rows.len());

    // -- venue rental: time_slots (~168) + bookings --------------------------
    // 4 venues × 3 weekly slots × 14 weeks (8 back + this week + 5 ahead),
    // anchored to this week's Monday. The per-slot index `s` derives from
    // the slot's *date* (num_days_from_ce), so a slot keeps its booking rule
    // forever — re-runs in later weeks can't reshuffle history. Past slots:
    // s%20 → 0-11 completed (60%) / 12-13 cancelled (10%) / 14 no_show (5%)
    // / 15-19 unbooked.
    let venue_ids = [
        venue_id_by_slug(db, "trampoline-zone").await?,
        venue_id_by_slug(db, "floor-gymnastics-zone").await?,
        venue_id_by_slug(db, "aerial-skills-zone").await?,
        venue_id_by_slug(db, "kids-play-zone").await?,
    ];
    let venue_prices: [i64; 4] = [80_000, 100_000, 130_000, 150_000];
    let slot_hours: [(u32, u32); 3] = [(10, 12), (14, 16), (19, 21)];
    let monday = today - Days::new(today.weekday().num_days_from_monday() as u64);
    let mut rental_slots = 0usize;
    let mut rental_bookings = 0usize;
    for w in 0..14i64 {
        let week_start = monday + Duration::weeks(w - 8);
        for v in 0..4usize {
            for (j, &(start_h, end_h)) in slot_hours.iter().enumerate() {
                let date = week_start + Days::new(((v + j * 2) % 7) as u64);
                let s = date.num_days_from_ce() as usize * 12 + v * 3 + j;
                let booking_status = if date < today {
                    match s % 20 {
                        0..=11 => Some(BookingStatus::Completed),
                        12..=13 => Some(BookingStatus::Cancelled),
                        14 => Some(BookingStatus::NoShow),
                        _ => None,
                    }
                } else {
                    None
                };
                let slot_id = upsert_time_slot(
                    db,
                    &TimeSlotSeed {
                        venue_id: venue_ids[v],
                        date,
                        start_time: NaiveTime::from_hms_opt(start_h, 0, 0).expect("valid hour"),
                        end_time: NaiveTime::from_hms_opt(end_h, 0, 0).expect("valid hour"),
                        capacity: 1,
                        price_cents: venue_prices[v],
                    },
                )
                .await?;
                rental_slots += 1;
                if let Some(status) = booking_status {
                    insert_booking_if_absent(
                        db,
                        member_ids[(s * 7) % 24],
                        slot_id,
                        status.as_str(),
                        venue_prices[v],
                        at_utc(date - Days::new(3), 8, at.now),
                    )
                    .await?;
                    rental_bookings += 1;
                }
            }
        }
    }
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
            if k == 0 { 1 + (raw % today.day() as usize) as u32 } else { 1 + (raw % 27) as u32 }
        };

        let trial_count = 8 + ((k as usize * 7 + 1) % 3); // 9 / 10 / 8
        for seq in 1..=trial_count {
            let category = CATEGORIES[(seq + k as usize) % 4];
            let parent_name = format!("試上家長{seq:02}");
            let phone = format!("09{:08}", (k as usize * 37 + seq * 13) % 100_000_000);
            let created_at =
                at_utc(month_first.with_day(clamp(seq * 5 + k as usize * 3)).expect("valid day"), 2, at.now);
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
            const SUBJECTS: [&str; 4] =
                ["課程費用詢問", "場地租借詢問", "會員方案詢問", "營業時間詢問"];
            let created_at =
                at_utc(month_first.with_day(clamp(seq * 7 + k as usize * 5)).expect("valid day"), 2, at.now);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_points_targets_match_the_legacy_formula() {
        // The 24 targets the old inline `match (i - 1) / 6` produced — the
        // PointsTier-derived formula must reproduce them exactly.
        let legacy: [i64; 24] = [
            150, 200, 250, 300, 350, 400, // <500
            700, 900, 1_100, 1_300, 1_500, 1_700, // 500-1999
            2_400, 2_800, 3_200, 3_600, 4_000, 4_400, // 2000-4999
            5_500, 6_000, 6_500, 7_000, 7_500, 8_000, // ≥5000
        ];
        for (i, expected) in (1..=24usize).zip(legacy) {
            assert_eq!(member_points_target(i), expected, "member {i}");
        }
    }

    #[test]
    fn member_points_targets_put_six_members_in_each_tier() {
        for &tier in PointsTier::ALL {
            let members = (1..=24usize)
                .filter(|&i| PointsTier::from_balance(member_points_target(i)) == tier)
                .count();
            assert_eq!(members, 6, "{tier:?}");
        }
    }
}
