use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;

use crate::config::ServerConfig;
use crate::error::AppError;

use super::dto::{
    AvailabilityQuery, CreateSlotsRequest, DaySchedule, ScheduleQuery, TimeSlotResponse,
};
use super::repository;

/// Max slot capacity — a sanity cap so a typo doesn't create a 2-billion-seat
/// slot. If a real-world use case needs more, raise this.
const MAX_SLOT_CAPACITY: i32 = 10_000;

fn studio_tz(server: &ServerConfig) -> Tz {
    // Startup validation (`AppConfig::load`) already rejects invalid
    // timezones, so by the time this runs we know the parse succeeds.
    // The `unwrap_or` is a belt-and-braces fallback that only fires if a
    // future refactor bypasses the startup check.
    server
        .studio_timezone
        .parse::<Tz>()
        .unwrap_or(chrono_tz::UTC)
}

/// Parse an `HH:MM` or `HH:MM:SS` time-of-day string. Accepting both formats
/// makes the API lenient to callers that send whatever their UI produces
/// (HTML `<input type="time">` for example sometimes emits seconds), without
/// forcing clients to strip trailing `:00`.
fn parse_time_of_day(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .ok()
}

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
    let date = NaiveDate::parse_from_str(&params.date, "%Y-%m-%d")
        .map_err(|_| AppError::BadRequest("invalid date format, expected YYYY-MM-DD".into()))?;

    let slots = repository::find_by_date(db, date).await?;
    Ok(slots.into_iter().map(TimeSlotResponse::from).collect())
}

pub async fn create_slots(
    db: &PgPool,
    server: &ServerConfig,
    req: CreateSlotsRequest,
) -> Result<Vec<TimeSlotResponse>, AppError> {
    let tz = studio_tz(server);
    let now_utc = Utc::now();

    let mut parsed_slots = Vec::with_capacity(req.slots.len());

    for entry in &req.slots {
        let date = NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
            .map_err(|_| AppError::BadRequest(format!("invalid date format: {}", entry.date)))?;
        let start_time = parse_time_of_day(&entry.start_time).ok_or_else(|| {
            AppError::BadRequest(format!("invalid start_time format: {}", entry.start_time))
        })?;
        let end_time = parse_time_of_day(&entry.end_time).ok_or_else(|| {
            AppError::BadRequest(format!("invalid end_time format: {}", entry.end_time))
        })?;

        if end_time <= start_time {
            return Err(AppError::BadRequest("end_time must be after start_time".into()));
        }
        if entry.capacity <= 0 || entry.capacity > MAX_SLOT_CAPACITY {
            return Err(AppError::BadRequest(format!(
                "capacity must be between 1 and {MAX_SLOT_CAPACITY}"
            )));
        }

        // Reject past slots. Interpret the naive (date, start_time) in the
        // configured studio timezone and refuse anything not strictly in
        // the future.
        let local = NaiveDateTime::new(date, start_time);
        let slot_utc = tz
            .from_local_datetime(&local)
            .single()
            .ok_or_else(|| {
                AppError::BadRequest("start_time falls on an ambiguous local time".into())
            })?
            .with_timezone(&Utc);
        if slot_utc <= now_utc {
            return Err(AppError::BadRequest(
                "cannot create a slot in the past".into(),
            ));
        }

        parsed_slots.push((date, start_time, end_time, entry.venue_id, entry.course_id, entry.capacity));
    }

    // `bulk_create` already wraps the UNNEST insert in its own transaction,
    // so a mid-batch failure leaves no half-materialised schedule behind.
    let slots = repository::bulk_create(db, &parsed_slots).await?;
    Ok(slots.into_iter().map(TimeSlotResponse::from).collect())
}
