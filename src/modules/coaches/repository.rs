use chrono::NaiveTime;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{ClockRecord, Coach, CoachSchedule};

pub async fn find_all_active(db: &PgPool) -> Result<Vec<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT c.id, c.user_id, u.name, c.title, c.bio, c.experience, c.specialties, \
         c.certifications, c.is_active, c.display_order, c.slug, c.photo_url, \
         c.created_at, c.updated_at \
         FROM coaches c \
         JOIN users u ON u.id = c.user_id \
         WHERE c.is_active = true \
         ORDER BY c.display_order, c.created_at",
    )
    .fetch_all(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT c.id, c.user_id, u.name, c.title, c.bio, c.experience, c.specialties, \
         c.certifications, c.is_active, c.display_order, c.slug, c.photo_url, \
         c.created_at, c.updated_at \
         FROM coaches c \
         JOIN users u ON u.id = c.user_id \
         WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn find_by_user_id(db: &PgPool, user_id: Uuid) -> Result<Option<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT c.id, c.user_id, u.name, c.title, c.bio, c.experience, c.specialties, \
         c.certifications, c.is_active, c.display_order, c.slug, c.photo_url, \
         c.created_at, c.updated_at \
         FROM coaches c \
         JOIN users u ON u.id = c.user_id \
         WHERE c.user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Takes an already-open transaction so `coaches::service::create_coach` can
/// insert the coach row and assign the `coach` role
/// (`auth::repository::assign_role_tx`) atomically in one commit — a
/// role-assignment failure must never leave an orphaned coach row (or vice
/// versa). `name` isn't a column of this table; the correlated subquery
/// pulls it from `users` in the same round trip — same idiom as
/// `courses::repository::create`'s `enrolled_count`/`waitlist_count`
/// subqueries.
#[allow(clippy::too_many_arguments)]
pub async fn insert_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    title: &str,
    bio: Option<&str>,
    experience: Option<&str>,
    specialties: &[String],
    certifications: &[String],
    is_active: bool,
    display_order: i32,
    slug: Option<&str>,
    photo_url: Option<&str>,
) -> Result<Coach, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "INSERT INTO coaches AS c (id, user_id, title, bio, experience, specialties, \
         certifications, is_active, display_order, slug, photo_url, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW(), NOW()) \
         RETURNING c.id, c.user_id, (SELECT u.name FROM users u WHERE u.id = c.user_id) AS name, \
         c.title, c.bio, c.experience, c.specialties, c.certifications, c.is_active, \
         c.display_order, c.slug, c.photo_url, c.created_at, c.updated_at",
    )
    .bind(user_id)
    .bind(title)
    .bind(bio)
    .bind(experience)
    .bind(specialties)
    .bind(certifications)
    .bind(is_active)
    .bind(display_order)
    .bind(slug)
    .bind(photo_url)
    .fetch_one(&mut **tx)
    .await
}

/// Partial (PATCH-style) update — every argument optional; `bio`/
/// `experience`/`slug`/`photo_url` are `Option<Option<T>>` so callers can
/// distinguish "don't touch" (`None`) from "set to NULL" (`Some(None)`)
/// from "set to value" (`Some(Some(v))`). Template: `courses::repository::update`.
/// Returns `Ok(None)` if `id` doesn't match any row (caller maps to 404).
#[allow(clippy::too_many_arguments)]
pub async fn update(
    db: &PgPool,
    id: Uuid,
    title: Option<&str>,
    bio: Option<Option<&str>>,
    experience: Option<Option<&str>>,
    specialties: Option<&[String]>,
    certifications: Option<&[String]>,
    is_active: Option<bool>,
    display_order: Option<i32>,
    slug: Option<Option<&str>>,
    photo_url: Option<Option<&str>>,
) -> Result<Option<Coach>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE coaches AS c SET updated_at = now()");

    if let Some(v) = title {
        qb.push(", title = ").push_bind(v);
    }
    if let Some(v) = bio {
        qb.push(", bio = ").push_bind(v);
    }
    if let Some(v) = experience {
        qb.push(", experience = ").push_bind(v);
    }
    if let Some(v) = specialties {
        qb.push(", specialties = ").push_bind(v);
    }
    if let Some(v) = certifications {
        qb.push(", certifications = ").push_bind(v);
    }
    if let Some(v) = is_active {
        qb.push(", is_active = ").push_bind(v);
    }
    if let Some(v) = display_order {
        qb.push(", display_order = ").push_bind(v);
    }
    if let Some(v) = slug {
        qb.push(", slug = ").push_bind(v);
    }
    if let Some(v) = photo_url {
        qb.push(", photo_url = ").push_bind(v);
    }

    qb.push(" WHERE c.id = ").push_bind(id);
    qb.push(
        " RETURNING c.id, c.user_id, (SELECT u.name FROM users u WHERE u.id = c.user_id) AS name, \
          c.title, c.bio, c.experience, c.specialties, c.certifications, c.is_active, \
          c.display_order, c.slug, c.photo_url, c.created_at, c.updated_at",
    );

    qb.build_query_as::<Coach>().fetch_optional(db).await
}

pub async fn find_schedules(
    db: &PgPool,
    coach_id: Uuid,
) -> Result<Vec<CoachSchedule>, sqlx::Error> {
    sqlx::query_as::<_, CoachSchedule>(
        "SELECT id, coach_id, day_of_week, start_time, end_time, is_available, created_at \
         FROM coach_schedules \
         WHERE coach_id = $1 \
         ORDER BY day_of_week, start_time",
    )
    .bind(coach_id)
    .fetch_all(db)
    .await
}

/// Replace all of a coach's weekly schedule rows (delete + insert within one
/// transaction). Each tuple is `(day_of_week, start_time, end_time,
/// is_available)` — already parsed/validated by the caller
/// (`coaches::service::parse_schedule_entries`). Mirrors
/// `sessions::repository::replace_slots_tx`'s pre-parsed-row contract.
pub async fn replace_schedules(
    db: &PgPool,
    coach_id: Uuid,
    schedules: &[(i16, NaiveTime, NaiveTime, bool)],
) -> Result<Vec<CoachSchedule>, sqlx::Error> {
    let mut tx = db.begin().await?;

    sqlx::query("DELETE FROM coach_schedules WHERE coach_id = $1")
        .bind(coach_id)
        .execute(&mut *tx)
        .await?;

    for (day_of_week, start_time, end_time, is_available) in schedules {
        sqlx::query(
            "INSERT INTO coach_schedules (id, coach_id, day_of_week, start_time, end_time, is_available, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, NOW())",
        )
        .bind(coach_id)
        .bind(day_of_week)
        .bind(start_time)
        .bind(end_time)
        .bind(is_available)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    // Return the newly inserted schedules
    find_schedules(db, coach_id).await
}

pub async fn clock_in(
    db: &PgPool,
    coach_id: Uuid,
    note: Option<&str>,
) -> Result<ClockRecord, sqlx::Error> {
    sqlx::query_as::<_, ClockRecord>(
        "INSERT INTO clock_records (id, coach_id, clock_in, note, created_at) \
         VALUES (gen_random_uuid(), $1, NOW(), $2, NOW()) RETURNING *",
    )
    .bind(coach_id)
    .bind(note)
    .fetch_one(db)
    .await
}

pub async fn clock_out(
    db: &PgPool,
    coach_id: Uuid,
) -> Result<Option<ClockRecord>, sqlx::Error> {
    sqlx::query_as::<_, ClockRecord>(
        "UPDATE clock_records SET clock_out = NOW() \
         WHERE coach_id = $1 AND clock_out IS NULL \
         RETURNING *",
    )
    .bind(coach_id)
    .fetch_optional(db)
    .await
}

pub async fn find_clock_records(
    db: &PgPool,
    coach_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<ClockRecord>, sqlx::Error> {
    sqlx::query_as::<_, ClockRecord>(
        "SELECT id, coach_id, clock_in, clock_out, note, created_at \
         FROM clock_records \
         WHERE coach_id = $1 \
         ORDER BY clock_in DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(coach_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}
