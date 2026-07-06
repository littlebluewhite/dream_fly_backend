//! Idempotent development seed data.
//!
//! Run with `cargo run --bin seed`. Loads configuration the same way
//! `main.rs` does (`AppConfig::load()` — `config/default.toml` →
//! `config/{APP_ENV}.toml` → `APP__*` env vars) and applies migrations before
//! seeding, so this works standalone against a freshly-`docker-compose up -d`
//! database with no other setup step.
//!
//! Every insert is `INSERT ... ON CONFLICT DO NOTHING` keyed on the table's
//! natural unique column (email / slug / code) — a bare `ON CONFLICT DO
//! NOTHING` (no explicit target) no-ops on ANY unique-constraint violation on
//! that table, so this is safe regardless of whether the unique constraint is
//! a plain column or a functional `LOWER(slug)` index. Running this binary
//! twice must therefore leave row counts unchanged on the second run.

use std::collections::HashMap;

use anyhow::Context;
use chrono::{DateTime, Duration, NaiveTime, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use dream_fly_backend::config::AppConfig;
use dream_fly_backend::utils::password;

/// Convert a fixed list of `&str` literals into the `Vec<String>` sqlx needs
/// to bind a Postgres `TEXT[]` column.
fn vs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// users + roles
// ---------------------------------------------------------------------------

/// Insert a user (idempotent on `email`) and return its id whether the row
/// was just inserted or already existed.
async fn upsert_user(
    db: &PgPool,
    email: &str,
    name: &str,
    plain_password: &str,
    points_balance: i64,
) -> anyhow::Result<Uuid> {
    let hash = password::hash_password(plain_password.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("hashing password for {email}: {e}"))?;

    sqlx::query(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, points_balance, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, $5, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(&hash)
    .bind(points_balance)
    .execute(db)
    .await
    .with_context(|| format!("insert user {email}"))?;

    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(db)
        .await
        .with_context(|| format!("fetch id for user {email}"))
}

/// Attach a role to a user (idempotent — mirrors `auth::repository::assign_role`).
async fn assign_role(db: &PgPool, user_id: Uuid, role_name: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO user_roles (user_id, role_id)
        SELECT $1, id FROM roles WHERE name = $2
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(role_name)
    .execute(db)
    .await
    .with_context(|| format!("assign role '{role_name}' to {user_id}"))?;
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
/// `(course_id, day_of_week, start_time)` unique constraint).
async fn insert_course_schedule_slot(
    db: &PgPool,
    course_id: Uuid,
    day_of_week: i16,
    start_time: &str,
    end_time: &str,
) -> anyhow::Result<()> {
    let start = NaiveTime::parse_from_str(start_time, "%H:%M")
        .with_context(|| format!("parse seed start_time '{start_time}'"))?;
    let end = NaiveTime::parse_from_str(end_time, "%H:%M")
        .with_context(|| format!("parse seed end_time '{end_time}'"))?;

    sqlx::query(
        r#"
        INSERT INTO course_schedule_slots (id, course_id, day_of_week, start_time, end_time, created_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(course_id)
    .bind(day_of_week)
    .bind(start)
    .bind(end)
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
async fn insert_post(db: &PgPool, author_id: Uuid, seed: &PostSeed) -> anyhow::Result<()> {
    let published_at: DateTime<Utc> = Utc::now() - Duration::days(seed.days_ago);

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
// verification helper — printed at the end of every run
// ---------------------------------------------------------------------------

async fn print_row_counts(db: &PgPool) -> anyhow::Result<()> {
    // Literal (label, query) pairs — kept as `&'static str` rather than a
    // `format!`-built string so sqlx's `SqlSafeStr` compile-time check (no
    // dynamic SQL strings) is satisfied without an `AssertSqlSafe` escape
    // hatch.
    const QUERIES: [(&str, &str); 8] = [
        ("users", "SELECT COUNT(*) FROM users"),
        ("coaches", "SELECT COUNT(*) FROM coaches"),
        ("courses", "SELECT COUNT(*) FROM courses"),
        ("products", "SELECT COUNT(*) FROM products"),
        ("coupons", "SELECT COUNT(*) FROM coupons"),
        ("rewards", "SELECT COUNT(*) FROM rewards"),
        ("posts", "SELECT COUNT(*) FROM posts"),
        ("venues", "SELECT COUNT(*) FROM venues"),
    ];
    println!("\n-- row counts --");
    for (table, sql) in QUERIES {
        let n: i64 = sqlx::query_scalar(sql).fetch_one(db).await?;
        println!("{table:<10} {n}");
    }
    Ok(())
}

/// Returns true if `app_env` (the value of `APP_ENV`) denotes production,
/// matched case-insensitively so `Production` / `PRODUCTION` can't slip past
/// the guard in `main` below. Extracted so it can be unit-tested without
/// spawning the binary.
fn is_production_env(app_env: &str) -> bool {
    app_env.eq_ignore_ascii_case("production")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Refuse to run against production: this binary unconditionally upserts
    // a known admin credential (admin@dreamfly.tw / Admin#2026), which must
    // never exist outside development/staging. Read `APP_ENV` the same way
    // `config::AppConfig::load` and `main.rs`'s `validate_production_config`
    // do, and check it before the config is loaded or any DB connection is
    // opened.
    let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
    if is_production_env(&app_env) {
        anyhow::bail!(
            "refusing to run: APP_ENV={app_env} looks like production. This binary seeds \
             known credentials (admin@dreamfly.tw / Admin#2026) and must never run against \
             a production database."
        );
    }

    let config = AppConfig::load().context(
        "failed to load configuration — check APP_ENV, config/*.toml overlays, and APP__* env vars",
    )?;

    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database.url)
        .await
        .context("failed to connect to PostgreSQL — check APP__DATABASE__URL and that the DB is reachable")?;

    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .context("failed to run database migrations")?;

    println!("Connected + migrated. Seeding dev data (idempotent, safe to re-run)...");

    // -- admin -----------------------------------------------------------
    let admin_id = upsert_user(&db, "admin@dreamfly.tw", "系統管理員", "Admin#2026", 0).await?;
    assign_role(&db, admin_id, "admin").await?;
    println!("[users]    admin ready: admin@dreamfly.tw / Admin#2026");

    // -- test member -------------------------------------------------------
    let member_id = upsert_user(&db, "member@dreamfly.tw", "測試會員", "Member#2026", 1250).await?;
    assign_role(&db, member_id, "member").await?;
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
        let user_id = upsert_user(&db, seed.email, seed.user_name, "Coach#2026", 0).await?;
        assign_role(&db, user_id, "coach").await?;
        let coach_id = upsert_coach(&db, user_id, seed).await?;
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
        },
    ];

    for seed in &course_seeds {
        let coach_id = *coach_ids
            .get(seed.coach_slug)
            .ok_or_else(|| anyhow::anyhow!("unknown coach slug '{}'", seed.coach_slug))?;
        insert_course(&db, seed, coach_id).await?;
    }
    println!("[courses]  {} courses ready", course_seeds.len());

    // -- course weekly schedule slots -----------------------------------------
    let mut slot_count = 0usize;
    for seed in &course_seeds {
        let course_id = course_id_by_slug(&db, seed.slug).await?;
        for (day_of_week, start_time, end_time) in seed.slots {
            insert_course_schedule_slot(&db, course_id, *day_of_week, start_time, end_time).await?;
            slot_count += 1;
        }
    }
    println!("[courses]  {slot_count} weekly schedule slots ready");

    // -- products / plans ----------------------------------------------------
    let product_seeds: [ProductSeed; 5] = [
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
    ];

    for seed in &product_seeds {
        insert_product(&db, seed).await?;
    }
    println!("[products] {} products/plans ready", product_seeds.len());

    // -- coupons ---------------------------------------------------------
    let coupon_seeds: [(&str, i64); 3] = [
        ("DREAMFLY100", 10_000),
        ("NEWYEAR500", 50_000),
        ("WELCOME50", 5_000),
    ];
    for (code, discount_cents) in coupon_seeds {
        insert_coupon(&db, code, discount_cents).await?;
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
        insert_reward_if_absent(&db, seed).await?;
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
        insert_post(&db, admin_id, seed).await?;
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
        insert_venue(&db, seed).await?;
    }
    println!("[venues]   {} venues ready", venue_seeds.len());

    print_row_counts(&db).await?;

    println!("\nSeed complete. Dev accounts:");
    println!("  admin:  admin@dreamfly.tw  / Admin#2026");
    println!("  member: member@dreamfly.tw / Member#2026 (points_balance=1250)");
    println!("  coach:  coach1..coach4@dreamfly.tw / Coach#2026");

    db.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_production_env_matches_case_insensitively() {
        assert!(is_production_env("production"));
        assert!(is_production_env("Production"));
        assert!(is_production_env("PRODUCTION"));
    }

    #[test]
    fn is_production_env_rejects_other_envs() {
        assert!(!is_production_env("development"));
        assert!(!is_production_env("staging"));
        assert!(!is_production_env(""));
    }
}
