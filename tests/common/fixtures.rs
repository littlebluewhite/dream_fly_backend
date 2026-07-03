//! Per-domain seed helpers used by HTTP integration tests.
//!
//! These functions insert the minimum rows a test needs using raw SQL so
//! the test can focus on the HTTP behavior it wants to exercise, instead
//! of going through service-layer creation endpoints that themselves need
//! an authenticated admin user.

#![allow(dead_code)]

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Insert a coach profile linked to the given user. Returns the coach id.
pub async fn seed_coach(db: &PgPool, user_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO coaches (id, user_id, title, bio, experience, specialties, certifications, is_active, display_order, created_at, updated_at)
        VALUES ($1, $2, $3, 'Test bio', '5 years', ARRAY['gymnastics'], ARRAY['cert-a'], true, 0, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(title)
    .execute(db)
    .await
    .expect("insert coach");
    id
}

/// Insert a published course with a unique slug derived from `name`.
pub async fn seed_course(db: &PgPool, name: &str, coach_id: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(name), &id.to_string()[..8]);
    sqlx::query(
        r#"
        INSERT INTO courses (id, name, slug, level, description, duration_minutes, price_cents, max_students, features, is_active, coach_id, created_at, updated_at)
        VALUES ($1, $2, $3, 'beginner'::course_level, 'Test course', 60, 50000, 12, ARRAY['drop-in'], true, $4, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(slug)
    .bind(coach_id)
    .execute(db)
    .await
    .expect("insert course");
    id
}

/// Insert a published course with a unique slug, a caller-chosen
/// `max_students` capacity, and a non-null `schedule_text`. Additive variant
/// of `seed_course` (which hardcodes `max_students = 12` and leaves
/// `schedule_text` NULL) for capacity-guard and enrolment-response tests.
pub async fn seed_course_with_capacity(
    db: &PgPool,
    name: &str,
    coach_id: Option<Uuid>,
    max_students: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    let slug = format!("{}-{}", slugify(name), &id.to_string()[..8]);
    sqlx::query(
        r#"
        INSERT INTO courses (id, name, slug, level, description, duration_minutes, price_cents, max_students, features, is_active, coach_id, schedule_text, created_at, updated_at)
        VALUES ($1, $2, $3, 'beginner'::course_level, 'Test course', 60, 50000, $5, ARRAY['drop-in'], true, $4, 'Mon/Wed 19:00', NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(slug)
    .bind(coach_id)
    .bind(max_students)
    .execute(db)
    .await
    .expect("insert course");
    id
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

/// Insert a time slot for a given course/venue on tomorrow at 10:00.
pub async fn seed_time_slot_full(
    db: &PgPool,
    course_id: Option<Uuid>,
    venue_id: Option<Uuid>,
    capacity: i32,
) -> Uuid {
    let id = Uuid::now_v7();
    let date = (Utc::now() + Duration::days(2)).date_naive();
    let start = chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap();
    let end = chrono::NaiveTime::from_hms_opt(11, 0, 0).unwrap();
    sqlx::query(
        r#"
        INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, booked, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 0, 'available'::slot_status, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(date)
    .bind(start)
    .bind(end)
    .bind(venue_id)
    .bind(course_id)
    .bind(capacity)
    .execute(db)
    .await
    .expect("insert time_slot");
    id
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

/// Insert a product with entitlement config (`product_type` + `valid_days` +
/// `session_count`). Compatible extension of `seed_product` (defined in
/// `tests/common/mod.rs`), which is hardcoded to `merchandise` and has no
/// entitlement fields — subscriptions tests need `ticket`/`membership`
/// products carrying `valid_days`/`session_count` instead.
pub async fn seed_entitlement_product(
    db: &PgPool,
    slug: &str,
    product_type: &str,
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
        VALUES ($1, $2, $3, $4::product_type, $5, '{}'::text[], false, $6, $7, true, NOW(), NOW())
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
    status: &str,
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
        VALUES ($1, $2, $3, $4::subscription_status, $9, $5, $6, $7, $8, $9, $9)
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

/// Insert an enrolment row directly (bypassing `enrol_from_purchase_tx`) so
/// HTTP tests can set up exact states — status and `enrolled_at` ordering —
/// without a real checkout. `order_id` is left NULL (a nullable FK; these
/// tests don't need a real order row). Returns the enrolment id.
pub async fn seed_enrolment(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
    status: &str,
    enrolled_at: DateTime<Utc>,
) -> Uuid {
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
    status: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO waitlist_entries (id, user_id, course_id, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4::waitlist_status, $5, $5)
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
