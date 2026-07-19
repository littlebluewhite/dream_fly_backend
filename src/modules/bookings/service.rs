use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::kafka::events::{BookingCancelledPayload, BookingCreatedPayload};
use crate::kafka::outbox;
use crate::modules::notifications::service as notify;
use crate::modules::schedule;
use crate::utils::studio_clock;

use super::dto::{BookingResponse, CreateBookingRequest, PaginatedBookingsResponse};
use super::repository;

pub async fn create_booking(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    user_id: Uuid,
    req: CreateBookingRequest,
    correlation_id: Option<String>,
) -> Result<BookingResponse, AppError> {
    let tz = studio_clock::studio_tz(server);

    // Everything happens inside one transaction so capacity and duplicate
    // checks share a consistent snapshot.
    let mut tx = db.begin().await?;

    // Atomically increment booked count (fails if full). Returns the slot
    // row so we can check the start time without a second SELECT.
    let slot = schedule::repository::increment_booked_tx(&mut tx, req.time_slot_id).await?;
    let slot = match slot {
        Some(s) => s,
        // `increment_booked_tx`'s WHERE clause folds two causes into one
        // `None` — already at capacity, or admin-closed (`is_closed`) — so
        // the message covers both; codex 抓到現行是 400 非 409,狀態碼不變。
        None => return Err(AppError::BadRequest("time slot is full or closed".into())),
    };

    // Reject bookings for slots that have already started. We interpret the
    // naïve (date, start_time) in the studio's local tz and compare to
    // Utc::now.
    studio_clock::require_not_started(
        tz,
        now,
        slot.date,
        slot.start_time,
        "time slot",
        AppError::BadRequest("cannot book a time slot that has already started".into()),
    )?;

    // The partial unique index `uq_bookings_user_slot_active` is the
    // authoritative duplicate guard — translate the unique-violation into a
    // friendly Conflict instead of racing on a pre-check SELECT.
    let booking = repository::create_tx(
        &mut tx,
        user_id,
        req.time_slot_id,
        req.note.as_deref(),
        slot.price_cents,
    )
    .await
    .map_err(|e| {
        AppError::conflict_on_unique(e, "you already have a booking for this time slot")
    })?;

    // Queue the booking_created event atomically with the booking row. The
    // background dispatcher publishes it to Kafka with at-least-once
    // semantics — no more silent loss if Kafka is down at checkout time.
    outbox::insert_domain_event_tx(
        &mut tx,
        BookingCreatedPayload {
            booking_id: booking.id,
            user_id: booking.user_id,
            time_slot_id: booking.time_slot_id,
        },
        correlation_id,
    )
    .await?;

    tx.commit().await?;

    // Post-commit: write an in-DB notification directly so the user gets
    // feedback without waiting for the outbox dispatcher tick.
    notify::booking_confirmed(booking.user_id, booking.id)
        .deliver(db)
        .await;

    Ok(BookingResponse::from(booking))
}

pub async fn cancel_booking(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    auth: &AuthUser,
    booking_id: Uuid,
    correlation_id: Option<String>,
) -> Result<BookingResponse, AppError> {
    let tz = studio_clock::studio_tz(server);

    // 1. Open the tx first so the ownership check, status check, 24-hour
    //    check, and conditional UPDATE all see a consistent snapshot.
    let mut tx = db.begin().await?;

    let booking = repository::find_by_id_tx(&mut tx, booking_id)
        .await?
        .ok_or_else(|| AppError::NotFound("booking not found".into()))?;

    // 2. Ownership or admin
    auth.owns_or_admin(booking.user_id, "you can only cancel your own bookings")?;

    // 3. State machine: only pending/confirmed bookings can be cancelled.
    //    Cancelling a Completed / NoShow booking would decrement the slot
    //    counter for a session that already happened.
    if !booking.status.is_cancellable() {
        return Err(AppError::BadRequest(format!(
            "booking in state '{}' cannot be cancelled",
            booking.status.as_str()
        )));
    }

    // 4. 24-hour rule (skipped for admins). Read the slot with a row lock
    //    inside the same tx so it cannot be rescheduled mid-check.
    if !auth.is_admin() {
        let slot = schedule::repository::find_by_id_tx(&mut tx, booking.time_slot_id)
            .await?
            .ok_or_else(|| AppError::NotFound("time slot not found".into()))?;

        let slot_utc = studio_clock::to_utc_checked(tz, slot.date, slot.start_time, "time slot")?;

        // Inline — the repo's only 24h-window check; extract a helper if a second appears.
        let hours_until = (slot_utc - now).num_hours();
        if hours_until < 24 {
            return Err(AppError::BadRequest(
                "cannot cancel within 24 hours of the scheduled time".into(),
            ));
        }
    }

    // 5. Conditional update. This is the authoritative race guard — if
    //    another concurrent cancel slipped through the pre-check, the
    //    `status <> 'cancelled'` clause makes it a no-op and we return 409.
    let updated = repository::cancel_if_active_tx(&mut tx, booking_id)
        .await?
        .ok_or_else(|| AppError::Conflict("booking is already cancelled".into()))?;

    schedule::repository::decrement_booked_tx(&mut tx, booking.time_slot_id).await?;

    outbox::insert_domain_event_tx(
        &mut tx,
        BookingCancelledPayload {
            booking_id: updated.id,
            user_id: updated.user_id,
            time_slot_id: updated.time_slot_id,
        },
        correlation_id,
    )
    .await?;

    tx.commit().await?;

    // Post-commit inline notification (independent of the outbox).
    notify::booking_cancelled(updated.user_id, updated.id)
        .deliver(db)
        .await;

    Ok(BookingResponse::from(updated))
}

pub async fn my_bookings(
    db: &PgPool,
    user_id: Uuid,
    pagination: &PaginationParams,
) -> Result<PaginatedBookingsResponse, AppError> {
    let total = repository::count_by_user(db, user_id).await?;
    let bookings =
        repository::find_by_user(db, user_id, pagination.limit(), pagination.offset()).await?;

    Ok(PaginatedBookingsResponse {
        bookings: bookings.into_iter().map(BookingResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

pub async fn list_all(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<PaginatedBookingsResponse, AppError> {
    let total = repository::count_all(db).await?;
    let bookings = repository::find_all(db, pagination.limit(), pagination.offset()).await?;

    Ok(PaginatedBookingsResponse {
        bookings: bookings.into_iter().map(BookingResponse::from).collect(),
        meta: pagination.meta(total),
    })
}
