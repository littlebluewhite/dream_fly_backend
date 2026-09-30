//! Per-domain seed helpers used by HTTP integration tests.
//!
//! These functions insert the minimum rows a test needs using raw SQL so
//! the test can focus on the HTTP behavior it wants to exercise, instead
//! of going through service-layer creation endpoints that themselves need
//! an authenticated admin user.

#![allow(dead_code)]

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::coaches::repository as coaches_repository;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::modules::points::model::PointReason;
use dream_fly_backend::modules::products::model::ProductType;
use dream_fly_backend::modules::subscriptions::model::SubscriptionStatus;
use dream_fly_backend::modules::waitlist::model::WaitlistStatus;

use super::{add_course_to_cart, add_to_cart, seed_member};

/// Insert a coach profile linked to the given user and attach the `coach`
/// role, in the same transaction. Returns the coach id.
///
/// Owner: delegates to `coaches::repository::insert_tx` /
/// `permissions::repository::assign_role_by_name` rather than hand-rolling
/// the `INSERT` — mirrors `seed_member` above.
pub async fn seed_coach(db: &PgPool, user_id: Uuid, title: &str) -> Uuid {
    let mut tx = db.begin().await.expect("begin tx");

    let coach = coaches_repository::insert_tx(
        &mut tx,
        user_id,
        title,
        Some("Test bio"),
        Some("5 years"),
        &[String::from("gymnastics")],
        &[String::from("cert-a")],
        true,
        0,
        None,
        None,
    )
    .await
    .expect("insert coach");

    // Witness discarded: this helper has never invalidated the role/active
    // cache either, same rationale as `seed_member`.
    let _ = permissions_repository::assign_role_by_name(&mut tx, user_id, "coach")
        .await
        .expect("assign coach role");

    tx.commit().await.expect("commit seed_coach");

    coach.id
}

/// Insert a published course with a unique slug derived from `name`
/// (`max_students = 12`, `schedule_text` NULL). Shorthand for
/// [`CourseSeed`] with only the coach set.
pub async fn seed_course(db: &PgPool, name: &str, coach_id: Option<Uuid>) -> Uuid {
    CourseSeed { coach_id, ..CourseSeed::new(name) }.insert(db).await
}

/// Builder for a published `courses` row with a unique slug derived from
/// `name`. Defaults: no coach, `max_students = 12`, `schedule_text` NULL —
/// set only what the test is about, e.g.
/// `CourseSeed::new("Full").max_students(1).insert(db)`.
pub struct CourseSeed<'a> {
    name: &'a str,
    coach_id: Option<Uuid>,
    max_students: i32,
    schedule_text: Option<&'a str>,
}

impl<'a> CourseSeed<'a> {
    pub fn new(name: &'a str) -> Self {
        Self { name, coach_id: None, max_students: 12, schedule_text: None }
    }

    pub fn coach(self, coach_id: Uuid) -> Self {
        Self { coach_id: Some(coach_id), ..self }
    }

    pub fn max_students(self, max_students: i32) -> Self {
        Self { max_students, ..self }
    }

    pub fn schedule_text(self, schedule_text: &'a str) -> Self {
        Self { schedule_text: Some(schedule_text), ..self }
    }

    /// Returns the course id.
    pub async fn insert(self, db: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        let slug = format!("{}-{}", slugify(self.name), &id.to_string()[..8]);
        sqlx::query(
            r#"
            INSERT INTO courses (id, name, slug, level, description, duration_minutes, price_cents, max_students, features, is_active, coach_id, schedule_text, created_at, updated_at)
            VALUES ($1, $2, $3, 'beginner'::course_level, 'Test course', 60, 50000, $4, ARRAY['drop-in'], true, $5, $6, NOW(), NOW())
            "#,
        )
        .bind(id)
        .bind(self.name)
        .bind(slug)
        .bind(self.max_students)
        .bind(self.coach_id)
        .bind(self.schedule_text)
        .execute(db)
        .await
        .expect("insert course");
        id
    }
}

/// Insert a course weekly schedule slot directly (bypassing the
/// `PATCH /courses/{id}` `schedule_slots` upsert), so sessions tests can set
/// up a course's weekly pattern without going through the courses HTTP/service
/// layer. `day_of_week` is 0=Sunday..6=Saturday (PostgreSQL `EXTRACT(DOW)`
/// convention — matches `sessions::calendar::materialize_range`). Returns
/// the slot id. Shorthand for [`SlotSeed`] with no venue.
pub async fn seed_course_schedule_slot(
    db: &PgPool,
    course_id: Uuid,
    day_of_week: i16,
    start_time: NaiveTime,
    end_time: NaiveTime,
) -> Uuid {
    SlotSeed::new(course_id, day_of_week, start_time, end_time).insert(db).await
}

/// Builder for a `course_schedule_slots` row (see
/// [`seed_course_schedule_slot`] for the `day_of_week` convention). `venue`
/// defaults to NULL; set it for tests of venue resolution/snapshots.
pub struct SlotSeed<'a> {
    course_id: Uuid,
    day_of_week: i16,
    start_time: NaiveTime,
    end_time: NaiveTime,
    venue: Option<&'a str>,
}

impl<'a> SlotSeed<'a> {
    pub fn new(
        course_id: Uuid,
        day_of_week: i16,
        start_time: NaiveTime,
        end_time: NaiveTime,
    ) -> Self {
        Self { course_id, day_of_week, start_time, end_time, venue: None }
    }

    pub fn venue(self, venue: &'a str) -> Self {
        Self { venue: Some(venue), ..self }
    }

    /// Returns the slot id.
    pub async fn insert(self, db: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO course_schedule_slots (id, course_id, day_of_week, start_time, end_time, venue, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            "#,
        )
        .bind(id)
        .bind(self.course_id)
        .bind(self.day_of_week)
        .bind(self.start_time)
        .bind(self.end_time)
        .bind(self.venue)
        .execute(db)
        .await
        .expect("insert course_schedule_slot");
        id
    }
}

/// Insert a `course_sessions` row directly (bypassing
/// `sessions::calendar::materialize_range`), so attendance tests get a
/// concrete session id without first setting up a weekly schedule slot.
/// Shorthand for [`SessionSeed`] with no venue snapshot.
pub async fn seed_course_session(
    db: &PgPool,
    course_id: Uuid,
    session_date: NaiveDate,
    start_time: NaiveTime,
    end_time: NaiveTime,
) -> Uuid {
    SessionSeed::new(course_id, session_date, start_time, end_time).insert(db).await
}

/// Builder for a `course_sessions` row. `venue` (the snapshot column)
/// defaults to NULL; setting it mirrors what `calendar::materialize_range`
/// writes for a slot that has a venue.
pub struct SessionSeed<'a> {
    course_id: Uuid,
    session_date: NaiveDate,
    start_time: NaiveTime,
    end_time: NaiveTime,
    venue: Option<&'a str>,
}

impl<'a> SessionSeed<'a> {
    pub fn new(
        course_id: Uuid,
        session_date: NaiveDate,
        start_time: NaiveTime,
        end_time: NaiveTime,
    ) -> Self {
        Self { course_id, session_date, start_time, end_time, venue: None }
    }

    pub fn venue(self, venue: &'a str) -> Self {
        Self { venue: Some(venue), ..self }
    }

    /// Returns the session id.
    pub async fn insert(self, db: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO course_sessions (id, course_id, session_date, start_time, end_time, venue, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            "#,
        )
        .bind(id)
        .bind(self.course_id)
        .bind(self.session_date)
        .bind(self.start_time)
        .bind(self.end_time)
        .bind(self.venue)
        .execute(db)
        .await
        .expect("insert course_session");
        id
    }
}

pub async fn seed_venue_category(db: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(name), &id.to_string()[..8]);
    sqlx::query(
        r#"
        INSERT INTO venue_categories (id, name, slug, icon, display_order, created_at)
        VALUES ($1, $2, $3, 'icon', 0, NOW())
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(slug)
    .execute(db)
    .await
    .expect("insert venue_category");
    id
}

pub async fn seed_venue(db: &PgPool, name: &str, category_id: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(name), &id.to_string()[..8]);
    sqlx::query(
        r#"
        INSERT INTO venues (id, category_id, name, slug, description, features, image_url, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, 'Test venue', ARRAY['mat','bar'], NULL, true, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(category_id)
    .bind(name)
    .bind(slug)
    .execute(db)
    .await
    .expect("insert venue");
    id
}

/// Insert a published post authored by `author_id`.
pub async fn seed_post(db: &PgPool, author_id: Uuid, title: &str, published: bool) -> Uuid {
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(title), &id.to_string()[..8]);
    sqlx::query(
        r#"
        INSERT INTO posts (id, author_id, title, slug, content, excerpt, category, status, published_at, created_at, updated_at)
        VALUES ($1, $2, $3, $4, 'Body', 'Excerpt', 'article'::post_category,
                CASE WHEN $5 THEN 'published'::post_status ELSE 'draft'::post_status END,
                CASE WHEN $5 THEN NOW() ELSE NULL END,
                NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(author_id)
    .bind(title)
    .bind(slug)
    .bind(published)
    .execute(db)
    .await
    .expect("insert post");
    id
}

/// Insert a notification row (bypassing the Kafka consumer path) for a user.
pub async fn seed_notification(db: &PgPool, user_id: Uuid, title: &str, is_read: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO notifications (id, user_id, type, title, message, is_read, metadata, created_at)
        VALUES ($1, $2, 'system'::notification_type, $3, 'Test message', $4, NULL, NOW())
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(title)
    .bind(is_read)
    .execute(db)
    .await
    .expect("insert notification");
    id
}

/// Builder for a `time_slots` row inserted directly (bypassing the schedule
/// service). Defaults: the day after tomorrow (+2 days — safely outside the
/// 24-hour cancellation window), 10:00–11:00, no course/venue, `booked = 0`.
/// `on`/`start` place the slot at an exact (date, start_time), e.g. inside
/// or outside the cancellation window. The end is start + 1 hour, clamped
/// to end-of-day instead of wrapping past midnight.
pub struct TimeSlotSeed {
    capacity: i32,
    date: NaiveDate,
    start: NaiveTime,
    course_id: Option<Uuid>,
    venue_id: Option<Uuid>,
}

impl TimeSlotSeed {
    pub fn new(capacity: i32) -> Self {
        Self {
            capacity,
            date: (Utc::now() + Duration::days(2)).date_naive(),
            start: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
            course_id: None,
            venue_id: None,
        }
    }

    pub fn on(self, date: NaiveDate) -> Self {
        Self { date, ..self }
    }

    pub fn start(self, start: NaiveTime) -> Self {
        Self { start, ..self }
    }

    pub fn course(self, course_id: Uuid) -> Self {
        Self { course_id: Some(course_id), ..self }
    }

    pub fn venue(self, venue_id: Uuid) -> Self {
        Self { venue_id: Some(venue_id), ..self }
    }

    /// Returns the slot id.
    pub async fn insert(self, db: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        // `overflowing_add_signed` wraps at midnight, which would violate the
        // `time_slots_time_order CHECK (end_time > start_time)` whenever
        // `start` lands in the last hour of the day (callers pass
        // wall-clock-derived starts, so any test run between 20:00 and 21:00
        // UTC used to hit this) — clamp to end-of-day instead of wrapping.
        // Tests only compare against `start_time`, so the exact clamped end
        // value is inconsequential.
        let (end, carry) = self.start.overflowing_add_signed(Duration::hours(1));
        let end = if carry != 0 {
            NaiveTime::from_hms_micro_opt(23, 59, 59, 999_999).unwrap()
        } else {
            end
        };
        sqlx::query(
            r#"
            INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, booked, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, 0, NOW(), NOW())
            "#,
        )
        .bind(id)
        .bind(self.date)
        .bind(self.start)
        .bind(end)
        .bind(self.venue_id)
        .bind(self.course_id)
        .bind(self.capacity)
        .execute(db)
        .await
        .expect("insert time_slot");
        id
    }
}

/// Insert a coupon row directly, bypassing the service layer so tests can
/// set fields the create endpoint doesn't expose (e.g. `is_active`).
pub async fn seed_coupon(
    db: &PgPool,
    code: &str,
    discount_cents: i64,
    is_active: bool,
    expires_at: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO coupons (id, code, discount_cents, is_active, expires_at, created_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        "#,
    )
    .bind(id)
    .bind(code)
    .bind(discount_cents)
    .bind(is_active)
    .bind(expires_at)
    .execute(db)
    .await
    .expect("insert coupon");
    id
}

/// One line of an [`OrderSeed`] order — either a product line
/// (quantity × unit price) or a course line (always quantity 1, mirroring
/// the `cart_items_course_qty` CHECK the real checkout flow inherits).
#[derive(Clone, Copy)]
pub enum SeedOrderLine {
    Product { product_id: Uuid, quantity: i32, unit_price_cents: i64 },
    Course { course_id: Uuid, unit_price_cents: i64 },
}

impl SeedOrderLine {
    fn subtotal_cents(&self) -> i64 {
        match self {
            Self::Product { quantity, unit_price_cents, .. } => unit_price_cents * *quantity as i64,
            Self::Course { unit_price_cents, .. } => *unit_price_cents,
        }
    }
}

/// Builder for an `orders` row plus its `order_items`, inserted directly via
/// SQL (bypassing `orders::service::checkout`) so tests can set an exact
/// `status` — including `pending`/`cancelled`/`refunded`, which checkout
/// never produces — and the revenue-report dimensions (`payment_method`,
/// `paid_at`). Defaults: no lines, `paid_at`/`payment_method` NULL,
/// `discount_cents = 0`. `total_cents` defaults to the pre-discount sum of
/// line subtotals (the reports' gross line income 口徑); set it explicitly
/// for a line-less order that only needs `orders.total_cents`.
/// `created_at = paid_at.unwrap_or(now)` so the two timestamps never
/// straddle a month boundary. `order_number` embeds the full UUID, not a
/// truncated prefix: UUIDv7's leading hex chars are a millisecond
/// timestamp, so same-millisecond seeds would otherwise collide on its
/// UNIQUE constraint.
pub struct OrderSeed<'a> {
    user_id: Uuid,
    status: OrderStatus,
    paid_at: Option<DateTime<Utc>>,
    payment_method: Option<&'a str>,
    lines: Vec<SeedOrderLine>,
    total_cents: Option<i64>,
}

impl<'a> OrderSeed<'a> {
    pub fn new(user_id: Uuid, status: OrderStatus) -> Self {
        Self {
            user_id,
            status,
            paid_at: None,
            payment_method: None,
            lines: Vec::new(),
            total_cents: None,
        }
    }

    pub fn paid_at(self, paid_at: DateTime<Utc>) -> Self {
        Self { paid_at: Some(paid_at), ..self }
    }

    pub fn payment_method(self, payment_method: &'a str) -> Self {
        Self { payment_method: Some(payment_method), ..self }
    }

    pub fn line(mut self, line: SeedOrderLine) -> Self {
        self.lines.push(line);
        self
    }

    pub fn total_cents(self, total_cents: i64) -> Self {
        Self { total_cents: Some(total_cents), ..self }
    }

    /// Returns the order id.
    pub async fn insert(self, db: &PgPool) -> Uuid {
        let order_id = Uuid::now_v7();
        let order_number = format!("TEST-{order_id}");
        let total_cents = self
            .total_cents
            .unwrap_or_else(|| self.lines.iter().map(SeedOrderLine::subtotal_cents).sum());
        let created_at = self.paid_at.unwrap_or_else(Utc::now);

        sqlx::query(
            r#"
            INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents, payment_method, paid_at, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, 0, $6, $7, $8, $8)
            "#,
        )
        .bind(order_id)
        .bind(self.user_id)
        .bind(&order_number)
        .bind(self.status)
        .bind(total_cents)
        .bind(self.payment_method)
        .bind(self.paid_at)
        .bind(created_at)
        .execute(db)
        .await
        .expect("insert order");

        for line in &self.lines {
            let (item_type, product_id, course_id, quantity, unit_price_cents) = match *line {
                SeedOrderLine::Product { product_id, quantity, unit_price_cents } => {
                    ("product", Some(product_id), None, quantity, unit_price_cents)
                }
                SeedOrderLine::Course { course_id, unit_price_cents } => {
                    ("course", None, Some(course_id), 1, unit_price_cents)
                }
            };
            sqlx::query(
                r#"
                INSERT INTO order_items (id, order_id, item_type, product_id, course_id, quantity, unit_price_cents, name, created_at)
                VALUES ($1, $2, $3::cart_item_type, $4, $5, $6, $7, 'Test Line', $8)
                "#,
            )
            .bind(Uuid::now_v7())
            .bind(order_id)
            .bind(item_type)
            .bind(product_id)
            .bind(course_id)
            .bind(quantity)
            .bind(unit_price_cents)
            .bind(created_at)
            .execute(db)
            .await
            .expect("insert order_item");
        }

        order_id
    }
}

/// Insert a booking row directly (bypassing `bookings::service::create`,
/// which always starts bookings `confirmed` at the slot's current price) so
/// venue-rental report tests can set an exact `status` — including
/// `cancelled`/`no_show`, which must NOT count as venue income — and an
/// exact `price_cents` snapshot independent of the slot's live price.
/// Keeps the `time_slots.booked` read cache consistent: a status that
/// `occupies_seat()` bumps the slot's `booked` in the same tx, so a later
/// cancel through the service decrements it back to where it started.
/// Returns the booking id.
pub async fn seed_booking(
    db: &PgPool,
    user_id: Uuid,
    time_slot_id: Uuid,
    status: BookingStatus,
    price_cents: i64,
) -> Uuid {
    let id = Uuid::now_v7();
    let occupies_seat = status.occupies_seat();
    let mut tx = db.begin().await.expect("begin tx");
    sqlx::query(
        r#"
        INSERT INTO bookings (id, user_id, time_slot_id, status, price_cents, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(time_slot_id)
    .bind(status)
    .bind(price_cents)
    .execute(&mut *tx)
    .await
    .expect("insert booking");
    if occupies_seat {
        sqlx::query("UPDATE time_slots SET booked = booked + 1 WHERE id = $1")
            .bind(time_slot_id)
            .execute(&mut *tx)
            .await
            .expect("bump time_slot booked");
    }
    tx.commit().await.expect("commit seed_booking");
    id
}

/// Insert a product with entitlement config (`product_type` + `valid_days` +
/// `session_count`). Compatible extension of `seed_product` (defined in
/// `tests/common/mod.rs`), which is hardcoded to `merchandise` and has no
/// entitlement fields — subscriptions tests need `ticket`/`membership`
/// products carrying `valid_days`/`session_count` instead.
pub async fn seed_entitlement_product(
    db: &PgPool,
    slug: &str,
    product_type: ProductType,
    price_cents: i64,
    valid_days: Option<i32>,
    session_count: Option<i32>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO products (
            id, name, slug, product_type, price_cents, features,
            is_highlighted, valid_days, session_count, is_active, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, '{}'::text[], false, $6, $7, true, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(format!("Test Entitlement Product {}", slug))
    .bind(slug)
    .bind(product_type)
    .bind(price_cents)
    .bind(valid_days)
    .bind(session_count)
    .execute(db)
    .await
    .expect("insert entitlement product");
    id
}

/// Insert a subscription row directly (bypassing `grant_from_purchase_tx`) so
/// redeem/status tests can set up exact states — remaining sessions,
/// expiry, or a cancelled status — the grant flow's own rule combinations
/// wouldn't otherwise produce. Returns the subscription id.
#[allow(clippy::too_many_arguments)]
pub async fn seed_subscription(
    db: &PgPool,
    user_id: Uuid,
    product_id: Uuid,
    status: SubscriptionStatus,
    expires_at: Option<DateTime<Utc>>,
    total_sessions: Option<i32>,
    remaining_sessions: Option<i32>,
    price_cents: i64,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO subscriptions (
            id, user_id, product_id, status, started_at, expires_at,
            total_sessions, remaining_sessions, price_cents, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $9, $5, $6, $7, $8, $9, $9)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(product_id)
    .bind(status)
    .bind(expires_at)
    .bind(total_sessions)
    .bind(remaining_sessions)
    .bind(price_cents)
    .bind(created_at)
    .execute(db)
    .await
    .expect("insert subscription");
    id
}

/// Insert an enrolment row directly (bypassing `enrol_batch_from_purchase_tx`) so
/// HTTP tests can set up exact states — status and `enrolled_at` ordering —
/// without a real checkout. `order_id` is left NULL (a nullable FK; these
/// tests don't need a real order row). Returns the enrolment id.
pub async fn seed_enrolment(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    status: EnrolmentStatus,
    enrolled_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO enrolments (id, user_id, course_id, order_id, status, enrolled_at, created_at, updated_at)
        VALUES ($1, $2, $3, NULL, $4, $5, $5, $5)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(course_id)
    .bind(status)
    .bind(enrolled_at)
    .execute(db)
    .await
    .expect("insert enrolment");
    id
}

/// Insert a waitlist entry row directly (bypassing the service's fullness
/// check) so tests can set up exact states — status and `created_at`
/// ordering — without needing a real full course. Returns the entry id.
pub async fn seed_waitlist_entry(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    status: WaitlistStatus,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO waitlist_entries (id, user_id, course_id, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $5)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(course_id)
    .bind(status)
    .bind(created_at)
    .execute(db)
    .await
    .expect("insert waitlist entry");
    id
}

/// Insert a point_ledger row directly (bypassing `points::service::apply_delta_tx`)
/// so tests can control `created_at` ordering and exact `reason`/`order_id`
/// combinations without a real balance mutation. Returns the entry id. Does
/// not touch `users.points_balance` — pair with `set_points_balance` when a
/// test also needs the balance to agree with the seeded ledger history.
pub async fn seed_point_ledger_entry(
    db: &PgPool,
    user_id: Uuid,
    delta: i64,
    balance_after: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO point_ledger (id, user_id, delta, balance_after, reason, order_id, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(delta)
    .bind(balance_after)
    .bind(reason)
    .bind(order_id)
    .bind(created_at)
    .execute(db)
    .await
    .expect("insert point_ledger entry");
    id
}

/// Directly set a user's `users.points_balance` (bypassing
/// `apply_delta_tx`) so tests can arrange an exact starting balance before
/// exercising a service call. Bypasses ledger bookkeeping entirely.
pub async fn set_points_balance(db: &PgPool, user_id: Uuid, balance: i64) {
    sqlx::query("UPDATE users SET points_balance = $2 WHERE id = $1")
        .bind(user_id)
        .bind(balance)
        .execute(db)
        .await
        .expect("set points balance");
}

/// Directly set a user's `users.birth_date` (the profile write path is
/// Task P4-B2's scope) so the admin report's age-bracket distribution tests
/// can place members in exact age buckets. `None` leaves/clears it NULL —
/// the "excluded from ageDist" case.
pub async fn set_birth_date(db: &PgPool, user_id: Uuid, birth_date: Option<NaiveDate>) {
    sqlx::query("UPDATE users SET birth_date = $2 WHERE id = $1")
        .bind(user_id)
        .bind(birth_date)
        .execute(db)
        .await
        .expect("set birth date");
}

/// Backdate a user's `created_at` so incidental fixture users don't leak
/// into the KPI "new members this/last month" buckets.
pub async fn backdate_user(db: &PgPool, user_id: Uuid, created_at: DateTime<Utc>) {
    sqlx::query("UPDATE users SET created_at = $2 WHERE id = $1")
        .bind(user_id)
        .bind(created_at)
        .execute(db)
        .await
        .expect("backdate user");
}

/// Insert an `attendance_records` row directly (bypassing `PUT
/// /sessions/{id}/attendance`), so tests can arrange present/absent/leave
/// combinations without a real coach HTTP round trip. Returns the
/// attendance record id.
pub async fn seed_attendance(
    db: &PgPool,
    session_id: Uuid,
    enrolment_id: Uuid,
    status: AttendanceStatus,
    marked_by: Uuid,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO attendance_records (id, session_id, enrolment_id, status, marked_by, marked_at, created_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(session_id)
    .bind(enrolment_id)
    .bind(status)
    .bind(marked_by)
    .execute(db)
    .await
    .expect("insert attendance_record");
    id
}

/// Insert a `leave_requests` row directly (bypassing
/// `leave::service::create_leave_request`), so tests can arrange exact
/// pre-existing states — `status` in particular — without going through the
/// "not yet started" / duplicate-index checks the create endpoint enforces.
/// `status` is one of `pending`/`approved`/`rejected`/`cancelled`. Returns
/// the new row's id.
pub async fn seed_leave_request(
    db: &PgPool,
    enrolment_id: Uuid,
    session_id: Uuid,
    status: LeaveStatus,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO leave_requests (id, enrolment_id, session_id, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(enrolment_id)
    .bind(session_id)
    .bind(status)
    .execute(db)
    .await
    .expect("insert leave_request");
    id
}

/// Directly set a leave request's `makeup_session_id` (bypassing the makeup
/// booking service and its seat/state checks) so tests can preset an existing
/// makeup link before exercising rebook and seat-model scenarios.
pub async fn set_makeup_session(db: &PgPool, leave_id: Uuid, makeup_session_id: Uuid) {
    sqlx::query("UPDATE leave_requests SET makeup_session_id = $2 WHERE id = $1")
        .bind(leave_id)
        .bind(makeup_session_id)
        .execute(db)
        .await
        .expect("set makeup_session_id");
}

/// Insert a `messages` row directly (bypassing
/// `messages::service::send_message`), so tests can control `sender_id`,
/// `read_at`, and `created_at` precisely — needed for unread-count,
/// mark-read, and pagination-ordering assertions that would otherwise race
/// against real wall-clock timestamps. Returns the new row's id.
pub async fn seed_message(
    db: &PgPool,
    conversation_id: Uuid,
    sender_id: Uuid,
    body: &str,
    read_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO messages (id, conversation_id, sender_id, body, created_at, read_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(conversation_id)
    .bind(sender_id)
    .bind(body)
    .bind(created_at)
    .bind(read_at)
    .execute(db)
    .await
    .expect("insert message");
    id
}

/// Insert a reward row directly, bypassing the service layer so tests can
/// set fields the create endpoint doesn't expose (e.g. `is_active`,
/// `display_order`). `stock = None` means unlimited. Returns the reward id.
pub async fn seed_reward(
    db: &PgPool,
    name: &str,
    points_cost: i32,
    stock: Option<i32>,
    is_active: bool,
    display_order: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO rewards (id, name, description, points_cost, stock, is_active, display_order, created_at, updated_at)
        VALUES ($1, $2, 'Test reward', $3, $4, $5, $6, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(points_cost)
    .bind(stock)
    .bind(is_active)
    .bind(display_order)
    .execute(db)
    .await
    .expect("insert reward");
    id
}

/// Small slug helper — lower, replace non-alnum with dashes.
pub fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ── 場景組合層(composite builders)──
//
// 以下 builders 不直接碰 SQL,只是把上面/父模組既有的 leaf builders 按常見
// 場景組裝成單一呼叫,收斂報表測試裡重複出現的多段式 inline arrange 序列。

/// `seed_member` + `backdate_user` 的組合,取代「建會員、再另呼叫
/// `backdate_user` 回填 created_at」的兩段式 inline 序列(報表 KPI 測試常
/// 需要把輔助帳號的建立時間排除在「本月新會員」統計之外)。密碼沿用測試
/// 共用密碼。Returns the user id.
pub async fn seed_member_created_at(db: &PgPool, email: &str, created_at: DateTime<Utc>) -> Uuid {
    let id = seed_member(db, email, "Password!234").await;
    backdate_user(db, id, created_at).await;
    id
}

/// [`seed_marked_attendance`] 回傳的新建 member/enrolment/session/
/// attendance 四個 id。
pub struct AttendanceScene {
    pub member: Uuid,
    pub enrolment: Uuid,
    pub session: Uuid,
    pub attendance: Uuid,
}

/// 「某課程在某日某時段有一筆已點名出勤」:`seed_member`(自動編號 email)
/// + `seed_enrolment`(active, now)+ `seed_course_session`(`start_time` 起
/// 一小時)+ `seed_attendance`(status, marked_by=member),取代四段式 inline
/// 序列。`start_time` 顯式入參:`course_sessions` 有 UNIQUE(course_id,
/// session_date, start_time),固定時間會讓同課同日的第二筆場景直接撞鍵,
/// 呼叫端需自行錯開。Returns the new scene.
pub async fn seed_marked_attendance(
    db: &PgPool,
    course_id: Uuid,
    session_date: NaiveDate,
    start_time: NaiveTime,
    status: AttendanceStatus,
) -> AttendanceScene {
    let email = format!("attendance-{}@example.com", Uuid::now_v7());
    let member = seed_member(db, &email, "Password!234").await;
    let enrolment =
        seed_enrolment(db, member, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let end_time = start_time + Duration::hours(1);
    let session = seed_course_session(db, course_id, session_date, start_time, end_time).await;
    let attendance = seed_attendance(db, session, enrolment, status, member).await;
    AttendanceScene { member, enrolment, session, attendance }
}

/// 一行式課程營收:[`OrderSeed`] 包一層,只塞一筆
/// `SeedOrderLine::Course`,消掉呼叫端重複的樣板。Returns the order id.
pub async fn seed_course_revenue(
    db: &PgPool,
    buyer: Uuid,
    course_id: Uuid,
    unit_price_cents: i64,
    status: OrderStatus,
    paid_at: Option<DateTime<Utc>>,
) -> Uuid {
    let mut order = OrderSeed::new(buyer, status);
    if let Some(paid_at) = paid_at {
        order = order.paid_at(paid_at);
    }
    order.line(SeedOrderLine::Course { course_id, unit_price_cents }).insert(db).await
}

/// 「某日一個時段被多筆不同狀態的預訂占用」:`TimeSlotSeed` 建一個
/// slot,`rentals` 每筆 (status, price_cents) 各自造一個新 member 再
/// `seed_booking`,取代「一個 slot + 逐筆各自 seed_member/seed_booking」的
/// inline 序列。Returns the new booking ids, in `rentals` order.
pub async fn seed_venue_rentals(
    db: &PgPool,
    slot_date: NaiveDate,
    rentals: &[(BookingStatus, i64)],
) -> Vec<Uuid> {
    let slot_id = TimeSlotSeed::new(rentals.len() as i32).on(slot_date).insert(db).await;
    let mut booking_ids = Vec::with_capacity(rentals.len());
    for (status, price_cents) in rentals {
        let email = format!("venue-rental-{}@example.com", Uuid::now_v7());
        let member = seed_member(db, &email, "Password!234").await;
        booking_ids.push(seed_booking(db, member, slot_id, status.clone(), *price_cents).await);
    }
    booking_ids
}

/// [`seed_leave_scene`] 回傳的新建 session/enrolment/leave 三個 id,外加呼叫
/// 端已持有的 member/course id 原樣附回——一次拿到完整場景,不必另外記混
/// 用不同來源的變數。
pub struct LeaveScene {
    pub member: Uuid,
    pub course: Uuid,
    pub session: Uuid,
    pub enrolment: Uuid,
    pub leave: Uuid,
}

/// 「某會員在某課程明日 09:00 場次請假(指定狀態)」:`seed_course_session`
/// (明日 09:00–10:00)+ `seed_enrolment`(active, now)+ `seed_leave_request`
/// (status),取代 `http_leave.rs` 反覆出現的五行儀式其中三行。`user_id`/
/// `course_id` 顯式入參,不像 [`seed_marked_attendance`] 自建新 member——
/// 請假測試常要「以請假本人的身分」打 owner-only 端點,呼叫端得自行決定用
/// `app.register_member`(需要 token)或 `common::seed_member`(純資料列)
/// 造這個人,composite 沒辦法代猜;`course_id` 同理,容量/coach 指派等變異
/// 呼叫端已有 `seed_course`/`CourseSeed` 可選。
/// `makeup_session_id` 給 `Some` 時,額外把它寫回
/// `leave_requests.makeup_session_id`——回補場次本身仍由呼叫端以
/// `seed_course_session` 建立,composite 只負責串接這最後一步。Returns the
/// new scene.
pub async fn seed_leave_scene(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    status: LeaveStatus,
    makeup_session_id: Option<Uuid>,
) -> LeaveScene {
    let tomorrow = (Utc::now() + Duration::days(1)).date_naive();
    let start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let end = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
    let session = seed_course_session(db, course_id, tomorrow, start, end).await;
    let enrolment =
        seed_enrolment(db, user_id, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let leave = seed_leave_request(db, enrolment, session, status).await;
    if let Some(makeup_id) = makeup_session_id {
        set_makeup_session(db, leave, makeup_id).await;
    }
    LeaveScene { member: user_id, course: course_id, session, enrolment, leave }
}

/// [`seed_full_course`] 回傳的課程 id,與坐滿座位的會員 id 列表。
pub struct FullCourse {
    pub course: Uuid,
    pub occupants: Vec<Uuid>,
}

/// 「某課程剛好坐滿」:`CourseSeed`(max_students,不掛
/// coach)+ 等量的 `seed_member`/`seed_enrolment`(active, now)各造一位占位
/// 會員填滿座位,取代「建課、逐一造會員、逐一造 active enrolment」的多段式
/// inline 序列。參數名沿用 `courses` 表欄位 `max_students`(CONTEXT.md 座位
/// 詞條——_Avoid_: capacity)。Returns the new course id and its occupants'
/// member ids, in creation order.
pub async fn seed_full_course(db: &PgPool, name: &str, max_students: i32) -> FullCourse {
    let course = CourseSeed::new(name).max_students(max_students).insert(db).await;
    let mut occupants = Vec::with_capacity(max_students as usize);
    for _ in 0..max_students {
        let email = format!("occupant-{}@example.com", Uuid::now_v7());
        let member = seed_member(db, &email, "Password!234").await;
        seed_enrolment(db, member, course, EnrolmentStatus::Active, Utc::now()).await;
        occupants.push(member);
    }
    FullCourse { course, occupants }
}

/// One line of a [`seed_carted_member`] cart — either a product line
/// (quantity) or a course line (always quantity 1, mirroring the
/// `cart_items_course_qty` CHECK). Mirrors [`SeedOrderLine`]'s shape but
/// carries no price field: cart lines don't snapshot a price the way order
/// lines do — checkout re-reads the live `products`/`courses` row at commit
/// time (`cart::repository::find_cart_items_for_checkout_tx`), so reusing
/// `SeedOrderLine` here would accept a `unit_price_cents` argument that
/// checkout silently ignores.
pub enum SeedCartLine {
    Product { product_id: Uuid, quantity: i32 },
    Course { course_id: Uuid },
}

/// 「某會員的購物車裝了一車東西,還帶著點數餘額」:`seed_member`+ 逐行
/// `add_to_cart`/`add_course_to_cart`(依 [`SeedCartLine`])+
/// `set_points_balance`,取代 checkout 測試裡「建會員、逐行加入購物車、設
/// 點數餘額」的多段式 inline 序列。`cart` 用 [`SeedCartLine`] 而非
/// [`SeedOrderLine`]——理由見該 enum 的文件。`points = 0` 等同不特別設定
/// (會員預設餘額本就是 0)。Returns the new member's id.
pub async fn seed_carted_member(
    db: &PgPool,
    email: &str,
    cart: &[SeedCartLine],
    points: i64,
) -> Uuid {
    let member = seed_member(db, email, "Password!234").await;
    for line in cart {
        match line {
            SeedCartLine::Product { product_id, quantity } => {
                add_to_cart(db, member, *product_id, *quantity).await
            }
            SeedCartLine::Course { course_id } => add_course_to_cart(db, member, *course_id).await,
        }
    }
    set_points_balance(db, member, points).await;
    member
}

/// [`seed_session_scene`] 回傳的新建 course/slot/session 三個 id。
pub struct SessionScene {
    pub course: Uuid,
    pub slot: Uuid,
    pub session: Uuid,
}

/// 「某課程在某天某時段有一筆對應週課表 slot 的場次」：`seed_course`
/// （name、coach_id）+ `seed_course_schedule_slot`（day_of_week 由
/// `session_date` 反推，不留給呼叫端另傳——與 `session_date` 兜不起來的
/// `day_of_week` 沒有意義，這兩者本就該連動，不留製造不一致的空間）+
/// `seed_course_session`（同一 `session_date`/`start_time`，結束時間固定
/// 「起算 +1 小時」，同 `seed_marked_attendance` 既有慣例：本檔目前每個
/// 場次皆整點起算 1 小時，沒有測試需要別的長度），取代
/// `service_sessions.rs` 反覆出現的「建課、建 slot」兩段式 inline 序列，並
/// 讓場次直接落地成資料列——不必依賴 ACT 呼叫（`list_course_sessions`/
/// `today_sessions`）自己的 materialize 副作用才拿得到一筆 `course_sessions`
/// 列。`name`/`coach_id` 顯式入參，同 `seed_course`——課名與是否指派教練皆
/// 是呼叫端才知道的變異點。`session_date`/`start_time` 顯式入參：測試常需
/// 要場次精確落在「今天」或某個未來/過去日期，composite 沒辦法代猜是哪一
/// 天。`venue` 給 `Some` 時同時落在 slot 與場次的 venue 快照上（同
/// `calendar::materialize_range` 物化時的寫法，供 venue 解析測試比對），`None` 兩
/// 者皆同原生的 NULL 預設。**不適用**於
/// 「slot 本身有無存在就是測試標的」（例如場次無對應 slot 時 venue 應為
/// null 的測試——需要真的沒有 slot）或「materialize 本身就是測試標的」
/// （例如 materialize 冪等測試——需要 ACT 呼叫自己把場次物化出來）的案
/// 例：本 composite 一定連帶造出 slot 與場次，硬套會讓那類測試失去意義。
/// Returns the new scene.
pub async fn seed_session_scene(
    db: &PgPool,
    name: &str,
    coach_id: Option<Uuid>,
    session_date: NaiveDate,
    start_time: NaiveTime,
    venue: Option<&str>,
) -> SessionScene {
    let course = seed_course(db, name, coach_id).await;
    let day_of_week = session_date.weekday().num_days_from_sunday() as i16;
    let end_time = start_time + Duration::hours(1);
    let mut slot = SlotSeed::new(course, day_of_week, start_time, end_time);
    let mut session = SessionSeed::new(course, session_date, start_time, end_time);
    if let Some(v) = venue {
        slot = slot.venue(v);
        session = session.venue(v);
    }
    let slot = slot.insert(db).await;
    let session = session.insert(db).await;
    SessionScene { course, slot, session }
}
