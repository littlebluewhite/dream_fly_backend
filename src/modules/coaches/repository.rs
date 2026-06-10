use chrono::NaiveTime;
use sqlx::PgPool;
use uuid::Uuid;

use super::dto::ScheduleEntry;
use super::model::{ClockRecord, Coach, CoachSchedule};

pub async fn find_all_active(db: &PgPool) -> Result<Vec<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT id, user_id, title, bio, experience, specialties, certifications, \
         is_active, display_order, created_at, updated_at \
         FROM coaches \
         WHERE is_active = true \
         ORDER BY display_order, created_at",
    )
    .fetch_all(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT id, user_id, title, bio, experience, specialties, certifications, \
         is_active, display_order, created_at, updated_at \
         FROM coaches WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn find_by_user_id(db: &PgPool, user_id: Uuid) -> Result<Option<Coach>, sqlx::Error> {
    sqlx::query_as::<_, Coach>(
        "SELECT id, user_id, title, bio, experience, specialties, certifications, \
         is_active, display_order, created_at, updated_at \
         FROM coaches WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
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

pub async fn replace_schedules(
    db: &PgPool,
    coach_id: Uuid,
    schedules: &[ScheduleEntry],
) -> Result<Vec<CoachSchedule>, sqlx::Error> {
    let mut tx = db.begin().await?;

    sqlx::query("DELETE FROM coach_schedules WHERE coach_id = $1")
        .bind(coach_id)
        .execute(&mut *tx)
        .await?;

    for entry in schedules {
        let start_time = NaiveTime::parse_from_str(&entry.start_time, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(&entry.start_time, "%H:%M:%S"))
            .map_err(|e| sqlx::Error::Protocol(format!("invalid start_time: {}", e)))?;
        let end_time = NaiveTime::parse_from_str(&entry.end_time, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(&entry.end_time, "%H:%M:%S"))
            .map_err(|e| sqlx::Error::Protocol(format!("invalid end_time: {}", e)))?;

        sqlx::query(
            "INSERT INTO coach_schedules (id, coach_id, day_of_week, start_time, end_time, is_available, created_at) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, NOW())",
        )
        .bind(coach_id)
        .bind(entry.day_of_week)
        .bind(start_time)
        .bind(end_time)
        .bind(entry.is_available)
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
