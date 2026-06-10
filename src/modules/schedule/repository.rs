use chrono::{NaiveDate, NaiveTime};
use sqlx::PgPool;
use uuid::Uuid;

use super::model::TimeSlot;

/// `(date, start_time, end_time, venue_id, course_id, capacity)` — input row
/// for [`bulk_create`]. Aliased to keep the signature readable.
pub type SlotRow = (NaiveDate, NaiveTime, NaiveTime, Option<Uuid>, Option<Uuid>, i32);

pub async fn find_by_month(
    db: &PgPool,
    year: i32,
    month: u32,
) -> Result<Vec<TimeSlot>, sqlx::Error> {
    // Caller already validates month/year ranges, but we still construct
    // dates with `from_ymd_opt` and fall back sanely on any overflow to
    // avoid panicking on an unexpected input.
    let first_day = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| sqlx::Error::Protocol("invalid year/month".into()))?;

    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(|| sqlx::Error::Protocol("invalid year/month".into()))?;

    let last_day = next_month_first
        .pred_opt()
        .ok_or_else(|| sqlx::Error::Protocol("invalid year/month".into()))?;

    sqlx::query_as::<_, TimeSlot>(
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, booked, \
         status, created_at, updated_at \
         FROM time_slots \
         WHERE date >= $1 AND date <= $2 \
         ORDER BY date, start_time",
    )
    .bind(first_day)
    .bind(last_day)
    .fetch_all(db)
    .await
}

pub async fn find_by_date(
    db: &PgPool,
    date: NaiveDate,
) -> Result<Vec<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, booked, \
         status, created_at, updated_at \
         FROM time_slots \
         WHERE date = $1 \
         ORDER BY start_time",
    )
    .bind(date)
    .fetch_all(db)
    .await
}

pub async fn find_by_id(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, booked, \
         status, created_at, updated_at \
         FROM time_slots \
         WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Transactional slot lookup with `FOR SHARE` so concurrent booking
/// mutations block until this read commits.
pub async fn find_by_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, booked, \
         status, created_at, updated_at \
         FROM time_slots \
         WHERE id = $1 \
         FOR SHARE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn bulk_create(
    db: &PgPool,
    slots: &[SlotRow],
) -> Result<Vec<TimeSlot>, sqlx::Error> {
    let mut tx = db.begin().await?;
    let out = bulk_create_tx(&mut tx, slots).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn bulk_create_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    slots: &[SlotRow],
) -> Result<Vec<TimeSlot>, sqlx::Error> {
    // Build a multi-row INSERT using UNNEST for efficiency
    let mut ids: Vec<Uuid> = Vec::with_capacity(slots.len());
    let mut dates: Vec<NaiveDate> = Vec::with_capacity(slots.len());
    let mut start_times: Vec<NaiveTime> = Vec::with_capacity(slots.len());
    let mut end_times: Vec<NaiveTime> = Vec::with_capacity(slots.len());
    let mut venue_ids: Vec<Option<Uuid>> = Vec::with_capacity(slots.len());
    let mut course_ids: Vec<Option<Uuid>> = Vec::with_capacity(slots.len());
    let mut capacities: Vec<i32> = Vec::with_capacity(slots.len());

    for (date, start_time, end_time, venue_id, course_id, capacity) in slots {
        ids.push(Uuid::now_v7());
        dates.push(*date);
        start_times.push(*start_time);
        end_times.push(*end_time);
        venue_ids.push(*venue_id);
        course_ids.push(*course_id);
        capacities.push(*capacity);
    }

    sqlx::query_as::<_, TimeSlot>(
        "INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, booked, status, created_at, updated_at) \
         SELECT * FROM UNNEST($1::uuid[], $2::date[], $3::time[], $4::time[], $5::uuid[], $6::uuid[], $7::int[], \
         ARRAY_FILL(0, ARRAY[$8::int])::int[], \
         ARRAY_FILL('available'::slot_status, ARRAY[$8::int])::slot_status[], \
         ARRAY_FILL(now(), ARRAY[$8::int])::timestamptz[], \
         ARRAY_FILL(now(), ARRAY[$8::int])::timestamptz[]) \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, booked, status, created_at, updated_at",
    )
    .bind(&ids)
    .bind(&dates)
    .bind(&start_times)
    .bind(&end_times)
    .bind(&venue_ids)
    .bind(&course_ids)
    .bind(&capacities)
    .bind(slots.len() as i32)
    .fetch_all(&mut **tx)
    .await
}

pub async fn increment_booked_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    slot_id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked + 1, \
         status = CASE \
           WHEN booked + 1 >= capacity THEN 'full'::slot_status \
           WHEN booked + 1 >= (capacity * 0.8)::int THEN 'limited'::slot_status \
           ELSE status \
         END, \
         updated_at = now() \
         WHERE id = $1 AND booked < capacity \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, booked, status, created_at, updated_at",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn decrement_booked_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    slot_id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked - 1, \
         status = CASE \
           WHEN booked - 1 < (capacity * 0.8)::int THEN 'available'::slot_status \
           ELSE status \
         END, \
         updated_at = now() \
         WHERE id = $1 AND booked > 0 \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, booked, status, created_at, updated_at",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await
}
