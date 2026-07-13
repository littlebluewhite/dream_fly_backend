//! Course 讀寫。以下每個 SELECT 重複出現的 `enrolled_count` correlated
//! subquery 是座位 COUNT 謂詞的**顯示用 inline 拷貝**;謂詞本身已下沉為
//! `active_enrolments` view(migration `20260711000001`)單一持有——多處
//! 拷貝共享同一份 view 定義,不再需要「先改 `courses::seats` 的謂詞、
//! 再人肉同步這些拷貝」的慣例。拷貝仍刻意保留 inline(不函式化、不共用
//! SQL const):函式化會把單查詢列表變成 N+1;共用 const 則需要 `format!`
//! 組裝,犧牲字串 SQL 的可 grep 性(deletion-test 裁決;見 `seats.rs` 模組
//! doc)。

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{Course, CourseLevel};

pub async fn find_all_active(
    db: &PgPool,
    limit: u32,
    offset: u32,
) -> Result<Vec<Course>, sqlx::Error> {
    sqlx::query_as::<_, Course>(
        "SELECT c.id, c.name, c.slug, c.level, c.description, c.duration_minutes, c.price_cents, \
         c.max_students, c.min_age, c.max_age, c.features, c.is_active, c.coach_id, c.category, \
         c.schedule_text, c.is_highlighted, c.created_at, c.updated_at, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = c.id) AS enrolled_count, \
         (SELECT COUNT(*) FROM waitlist_entries w WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count \
         FROM courses c WHERE c.is_active = true ORDER BY c.name \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_active(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM courses WHERE is_active = true")
        .fetch_one(db)
        .await
}

pub async fn find_by_slug(db: &PgPool, slug: &str) -> Result<Option<Course>, sqlx::Error> {
    sqlx::query_as::<_, Course>(
        "SELECT c.id, c.name, c.slug, c.level, c.description, c.duration_minutes, c.price_cents, \
         c.max_students, c.min_age, c.max_age, c.features, c.is_active, c.coach_id, c.category, \
         c.schedule_text, c.is_highlighted, c.created_at, c.updated_at, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = c.id) AS enrolled_count, \
         (SELECT COUNT(*) FROM waitlist_entries w WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count \
         FROM courses c WHERE LOWER(c.slug) = LOWER($1)",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Course>, sqlx::Error> {
    sqlx::query_as::<_, Course>(
        "SELECT c.id, c.name, c.slug, c.level, c.description, c.duration_minutes, c.price_cents, \
         c.max_students, c.min_age, c.max_age, c.features, c.is_active, c.coach_id, c.category, \
         c.schedule_text, c.is_highlighted, c.created_at, c.updated_at, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = c.id) AS enrolled_count, \
         (SELECT COUNT(*) FROM waitlist_entries w WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count \
         FROM courses c WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Takes an already-open transaction (rather than `&PgPool`) so
/// `courses::service` can insert the course row and, when the request
/// carries `schedule_slots`, replace the course's weekly slots
/// (`sessions::repository::replace_slots_tx`) atomically in one commit.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    slug: &str,
    level: &CourseLevel,
    description: Option<&str>,
    duration_minutes: i32,
    price_cents: i64,
    max_students: i32,
    min_age: Option<i32>,
    max_age: Option<i32>,
    features: &[String],
    coach_id: Option<Uuid>,
    category: Option<&str>,
    schedule_text: Option<&str>,
    is_highlighted: bool,
) -> Result<Course, sqlx::Error> {
    sqlx::query_as::<_, Course>(
        "INSERT INTO courses AS c (id, name, slug, level, description, duration_minutes, price_cents, \
         max_students, min_age, max_age, features, coach_id, category, schedule_text, is_highlighted, \
         created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3::course_level, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, now(), now()) \
         RETURNING c.id, c.name, c.slug, c.level, c.description, c.duration_minutes, c.price_cents, \
         c.max_students, c.min_age, c.max_age, c.features, c.is_active, c.coach_id, c.category, \
         c.schedule_text, c.is_highlighted, c.created_at, c.updated_at, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = c.id) AS enrolled_count, \
         (SELECT COUNT(*) FROM waitlist_entries w WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count",
    )
    .bind(name)
    .bind(slug)
    .bind(level.as_str())
    .bind(description)
    .bind(duration_minutes)
    .bind(price_cents)
    .bind(max_students)
    .bind(min_age)
    .bind(max_age)
    .bind(features)
    .bind(coach_id)
    .bind(category)
    .bind(schedule_text)
    .bind(is_highlighted)
    .fetch_one(&mut **tx)
    .await
}

/// Takes an already-open transaction — see [`create`]'s doc comment.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    name: Option<&str>,
    slug: Option<&str>,
    level: Option<&str>,
    description: Option<&str>,
    duration_minutes: Option<i32>,
    price_cents: Option<i64>,
    max_students: Option<i32>,
    min_age: Option<Option<i32>>,
    max_age: Option<Option<i32>>,
    features: Option<&[String]>,
    coach_id: Option<Option<Uuid>>,
    category: Option<Option<&str>>,
    schedule_text: Option<Option<&str>>,
    is_highlighted: Option<bool>,
) -> Result<Option<Course>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE courses AS c SET updated_at = now()");

    if let Some(v) = name {
        qb.push(", name = ").push_bind(v);
    }
    if let Some(v) = slug {
        qb.push(", slug = ").push_bind(v);
    }
    if let Some(v) = level {
        qb.push(", level = ").push_bind(v).push("::course_level");
    }
    if let Some(v) = description {
        qb.push(", description = ").push_bind(v);
    }
    if let Some(v) = duration_minutes {
        qb.push(", duration_minutes = ").push_bind(v);
    }
    if let Some(v) = price_cents {
        qb.push(", price_cents = ").push_bind(v);
    }
    if let Some(v) = max_students {
        qb.push(", max_students = ").push_bind(v);
    }
    if let Some(v) = min_age {
        qb.push(", min_age = ").push_bind(v);
    }
    if let Some(v) = max_age {
        qb.push(", max_age = ").push_bind(v);
    }
    if let Some(v) = features {
        qb.push(", features = ").push_bind(v);
    }
    if let Some(v) = coach_id {
        qb.push(", coach_id = ").push_bind(v);
    }
    if let Some(v) = category {
        qb.push(", category = ").push_bind(v);
    }
    if let Some(v) = schedule_text {
        qb.push(", schedule_text = ").push_bind(v);
    }
    if let Some(v) = is_highlighted {
        qb.push(", is_highlighted = ").push_bind(v);
    }

    qb.push(" WHERE c.id = ").push_bind(id);
    qb.push(
        " RETURNING c.id, c.name, c.slug, c.level, c.description, c.duration_minutes, c.price_cents, \
          c.max_students, c.min_age, c.max_age, c.features, c.is_active, c.coach_id, c.category, \
          c.schedule_text, c.is_highlighted, c.created_at, c.updated_at, \
          (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = c.id) AS enrolled_count, \
          (SELECT COUNT(*) FROM waitlist_entries w WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count",
    );

    qb.build_query_as::<Course>().fetch_optional(&mut **tx).await
}
