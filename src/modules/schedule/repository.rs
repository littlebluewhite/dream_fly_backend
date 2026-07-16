use chrono::{NaiveDate, NaiveTime};
use sqlx::PgPool;
use uuid::Uuid;

use crate::utils::studio_clock;

use super::model::TimeSlot;

/// `(date, start_time, end_time, venue_id, course_id, capacity, price_cents)`
/// — input row for [`bulk_create`]. Aliased to keep the signature readable.
pub type SlotRow = (NaiveDate, NaiveTime, NaiveTime, Option<Uuid>, Option<Uuid>, i32, i64);

pub async fn find_by_month(
    db: &PgPool,
    year: i32,
    month: u32,
) -> Result<Vec<TimeSlot>, sqlx::Error> {
    // Caller already validates month/year ranges, but `month_bounds` still
    // falls back to `None` (mapped to this error) on any overflow, rather
    // than panicking, as a defensive backstop.
    let (first_day, last_day) = studio_clock::month_bounds(year, month)
        .ok_or_else(|| sqlx::Error::Protocol("invalid year/month".into()))?;

    sqlx::query_as::<_, TimeSlot>(
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, \
         booked, is_closed, created_at, updated_at \
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
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, \
         booked, is_closed, created_at, updated_at \
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
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, \
         booked, is_closed, created_at, updated_at \
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
        "SELECT id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, \
         booked, is_closed, created_at, updated_at \
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
    let mut price_cents_vec: Vec<i64> = Vec::with_capacity(slots.len());

    for (date, start_time, end_time, venue_id, course_id, capacity, price_cents) in slots {
        ids.push(Uuid::now_v7());
        dates.push(*date);
        start_times.push(*start_time);
        end_times.push(*end_time);
        venue_ids.push(*venue_id);
        course_ids.push(*course_id);
        capacities.push(*capacity);
        price_cents_vec.push(*price_cents);
    }

    sqlx::query_as::<_, TimeSlot>(
        "INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, created_at, updated_at) \
         SELECT * FROM UNNEST($1::uuid[], $2::date[], $3::time[], $4::time[], $5::uuid[], $6::uuid[], $7::int[], $8::bigint[], \
         ARRAY_FILL(0, ARRAY[$9::int])::int[], \
         ARRAY_FILL(now(), ARRAY[$9::int])::timestamptz[], \
         ARRAY_FILL(now(), ARRAY[$9::int])::timestamptz[]) \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(&ids)
    .bind(&dates)
    .bind(&start_times)
    .bind(&end_times)
    .bind(&venue_ids)
    .bind(&course_ids)
    .bind(&capacities)
    .bind(&price_cents_vec)
    .bind(slots.len() as i32)
    .fetch_all(&mut **tx)
    .await
}

/// Atomically increments `booked` (fails — returns `None` — if the slot is
/// already full *or* admin-closed). `status` is no longer written here: it's
/// derived at read time from the resulting `booked`/`capacity`/`is_closed`
/// (see [`super::model::SlotStatus::derive`]), so this only needs to touch
/// the facts. `AND is_closed = false` is the gate that makes the admin
/// `is_closed` flag actually block new bookings — see
/// `bookings::service::create_booking`'s `None` branch.
pub async fn increment_booked_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    slot_id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked + 1, \
         updated_at = now() \
         WHERE id = $1 AND booked < capacity AND is_closed = false \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await
}

/// Atomically decrements `booked` (fails — returns `None` — if it's already
/// zero). No `is_closed` gate: cancelling an existing booking must always be
/// allowed to release its seat, even on a slot an admin closed afterwards.
pub async fn decrement_booked_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    slot_id: Uuid,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         booked = booked - 1, \
         updated_at = now() \
         WHERE id = $1 AND booked > 0 \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(slot_id)
    .fetch_optional(&mut **tx)
    .await
}

/// `PATCH /schedule/slots/{id}` — admin sets/clears the closed intent flag.
/// `None` = slot not found.
pub async fn set_closed(
    db: &PgPool,
    id: Uuid,
    is_closed: bool,
) -> Result<Option<TimeSlot>, sqlx::Error> {
    sqlx::query_as::<_, TimeSlot>(
        "UPDATE time_slots SET \
         is_closed = $2, \
         updated_at = now() \
         WHERE id = $1 \
         RETURNING id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, booked, is_closed, created_at, updated_at",
    )
    .bind(id)
    .bind(is_closed)
    .fetch_optional(db)
    .await
}
