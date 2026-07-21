use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::utils::studio_clock;

use super::dto::{
    AvailabilityQuery, CreateSlotsRequest, DaySchedule, ScheduleQuery, TimeSlotResponse,
    UpdateSlotRequest,
};
use super::repository;

/// Max slot capacity — a sanity cap so a typo doesn't create a 2-billion-seat
/// slot. If a real-world use case needs more, raise this.
const MAX_SLOT_CAPACITY: i32 = 10_000;

pub async fn get_monthly_schedule(
    db: &PgPool,
    params: ScheduleQuery,
) -> Result<Vec<DaySchedule>, AppError> {
    if !(1..=12).contains(&params.month) {
        return Err(AppError::BadRequest("month must be between 1 and 12".into()));
    }
    if !(1970..=2100).contains(&params.year) {
        return Err(AppError::BadRequest("year must be between 1970 and 2100".into()));
    }

    let slots = repository::find_by_month(db, params.year, params.month).await?;

    // Group slots by date
    let mut days: Vec<DaySchedule> = Vec::new();

    for slot in slots {
        let slot_date = slot.date;
        let response = TimeSlotResponse::from(slot);

        if let Some(day) = days.last_mut().filter(|d| d.date == slot_date) {
            day.slots.push(response);
        } else {
            days.push(DaySchedule {
                date: slot_date,
                slots: vec![response],
            });
        }
    }

    Ok(days)
}

pub async fn get_availability(
    db: &PgPool,
    params: AvailabilityQuery,
) -> Result<Vec<TimeSlotResponse>, AppError> {
    let date = studio_clock::parse_date(&params.date)
        .ok_or_else(|| AppError::BadRequest("invalid date format, expected YYYY-MM-DD".into()))?;

    let slots = repository::find_by_date(db, date).await?;
    Ok(slots.into_iter().map(TimeSlotResponse::from).collect())
}

pub async fn create_slots(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    req: CreateSlotsRequest,
) -> Result<Vec<TimeSlotResponse>, AppError> {
    let tz = studio_clock::studio_tz(server);

    let mut parsed_slots = Vec::with_capacity(req.slots.len());

    for entry in &req.slots {
        let date = studio_clock::parse_date(&entry.date)
            .ok_or_else(|| AppError::BadRequest(format!("invalid date format: {}", entry.date)))?;
        let start_time = studio_clock::parse_time_of_day(&entry.start_time).ok_or_else(|| {
            AppError::BadRequest(format!("invalid start_time format: {}", entry.start_time))
        })?;
        let end_time = studio_clock::parse_time_of_day(&entry.end_time).ok_or_else(|| {
            AppError::BadRequest(format!("invalid end_time format: {}", entry.end_time))
        })?;

        studio_clock::validate_time_window(start_time, end_time)?;
        if entry.capacity <= 0 || entry.capacity > MAX_SLOT_CAPACITY {
            return Err(AppError::BadRequest(format!(
                "capacity must be between 1 and {MAX_SLOT_CAPACITY}"
            )));
        }

        // Reject past slots. Interpret the naive (date, start_time) in the
        // configured studio timezone and refuse anything not strictly in
        // the future.
        studio_clock::require_not_started(
            tz,
            now,
            date,
            start_time,
            "start_time",
            AppError::BadRequest("cannot create a slot in the past".into()),
        )?;

        parsed_slots.push((
            date,
            start_time,
            end_time,
            entry.venue_id,
            entry.course_id,
            entry.capacity,
            entry.price_cents.unwrap_or(0),
        ));
    }

    // `bulk_create` already wraps the UNNEST insert in its own transaction,
    // so a mid-batch failure leaves no half-materialised schedule behind.
    let slots = repository::bulk_create(db, &parsed_slots)
        .await
        .map_err(|e| AppError::conflict_on_exclusion(e, "場地時段與既有時段重疊"))?;
    Ok(slots.into_iter().map(TimeSlotResponse::from).collect())
}

/// `PATCH /schedule/slots/{id}` — admin only (checked by the handler via
/// `admin_router()`). Sets/clears the `is_closed` intent flag; the derived
/// `status` in the response reflects it immediately (`closed` takes
/// priority over the booked/capacity computation — see
/// `SlotStatus::derive`).
pub async fn update_slot(
    db: &PgPool,
    id: Uuid,
    req: &UpdateSlotRequest,
) -> Result<TimeSlotResponse, AppError> {
    let slot = repository::set_closed(db, id, req.is_closed)
        .await?
        .ok_or_else(|| AppError::NotFound("time slot not found".into()))?;
    Ok(TimeSlotResponse::from(slot))
}
