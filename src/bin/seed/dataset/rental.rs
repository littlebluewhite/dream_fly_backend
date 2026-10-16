//! 場租佔位 (Venue-Rental Occupancy), seed side: `seed_rentals` lays out the
//! rental `time_slots` and the past slots' `bookings` directly, a documented
//! ADR-0010 bypass of `bookings::occupancy` (no cached count to sync —
//! occupancy is counted at read time from `occupying_bookings`). The rules
//! (slots per week, booking status keyed by the slot's date, never above
//! capacity, a rerun adds nothing) are in the GLOSSARY entry 「場租佔位」;
//! `run` looks the venues up by slug and hands them in.

use anyhow::Context;
use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::utils::studio_clock::StudioNow;

use super::at_utc;

/// One venue-rental time slot, column shape per
/// `schedule::repository::bulk_create_tx` (course_id NULL — pure rental).
struct TimeSlotSeed {
    venue_id: Uuid,
    date: NaiveDate,
    start_time: NaiveTime,
    end_time: NaiveTime,
    capacity: i32,
    price_cents: i64,
}

/// Lay out the rental calendar for `venues` (`(venue_id, price_cents)`, in
/// `run`'s fixed order) and book its past slots for `members`; returns
/// `(slots, bookings)` touched this run, existing rows included.
///
/// Each venue gets 3 weekly slots × 14 weeks (8 back + this week + 5 ahead),
/// anchored to this week's Monday, capacity 1. The per-slot index `s` derives
/// from the slot's *date* (num_days_from_ce), so a slot keeps its booking rule
/// forever — re-runs in later weeks can't reshuffle history. Past slots:
/// s%20 → 0-11 completed (60%) / 12-13 cancelled (10%) / 14 no_show (5%) /
/// 15-19 unbooked; the booker is `members[(s * 7) % members.len()]`, booked
/// 3 days ahead at the slot's price.
pub(super) async fn seed_rentals(
    db: &PgPool,
    at: StudioNow,
    venues: &[(Uuid, i64)],
    members: &[Uuid],
) -> anyhow::Result<(usize, usize)> {
    let today = at.today();
    let slot_hours: [(u32, u32); 3] = [(10, 12), (14, 16), (19, 21)];
    let monday = today - Days::new(today.weekday().num_days_from_monday() as u64);
    let mut rental_slots = 0usize;
    let mut rental_bookings = 0usize;
    for w in 0..14i64 {
        let week_start = monday + Duration::weeks(w - 8);
        for (v, &(venue_id, price_cents)) in venues.iter().enumerate() {
            for (j, &(start_h, end_h)) in slot_hours.iter().enumerate() {
                let date = week_start + Days::new(((v + j * 2) % 7) as u64);
                let s = date.num_days_from_ce() as usize * 12 + v * 3 + j;
                let booking_status = if date < today {
                    match s % 20 {
                        0..=11 => Some(BookingStatus::Completed),
                        12..=13 => Some(BookingStatus::Cancelled),
                        14 => Some(BookingStatus::NoShow),
                        _ => None,
                    }
                } else {
                    None
                };
                let slot_id = upsert_time_slot(
                    db,
                    &TimeSlotSeed {
                        venue_id,
                        date,
                        start_time: NaiveTime::from_hms_opt(start_h, 0, 0).expect("valid hour"),
                        end_time: NaiveTime::from_hms_opt(end_h, 0, 0).expect("valid hour"),
                        capacity: 1,
                        price_cents,
                    },
                )
                .await?;
                rental_slots += 1;
                if let Some(status) = booking_status {
                    insert_booking_if_absent(
                        db,
                        members[(s * 7) % members.len()],
                        slot_id,
                        status,
                        price_cents,
                        at_utc(date - Days::new(3), 8, at.now),
                    )
                    .await?;
                    rental_bookings += 1;
                }
            }
        }
    }
    Ok((rental_slots, rental_bookings))
}

/// Insert a rental slot (idempotent by existence check on
/// `(venue_id, date, start_time)` — the table's only guard is the GIST
/// anti-overlap EXCLUDE, which bare ON CONFLICT DO NOTHING also covers as a
/// belt) and return its id either way. The slot stores no occupancy count:
/// its `booked` is counted at read time from the `occupying_bookings` view
/// (ADR-0015), so a slot that crosses into the past and gains a booking on a
/// later run needs no sync.
async fn upsert_time_slot(db: &PgPool, seed: &TimeSlotSeed) -> anyhow::Result<Uuid> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM time_slots WHERE venue_id = $1 AND date = $2 AND start_time = $3",
    )
    .bind(seed.venue_id)
    .bind(seed.date)
    .bind(seed.start_time)
    .fetch_optional(db)
    .await
    .context("check existing time_slot")?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO time_slots (id, date, start_time, end_time, venue_id, course_id, capacity, price_cents, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, NOW(), NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(id)
    .bind(seed.date)
    .bind(seed.start_time)
    .bind(seed.end_time)
    .bind(seed.venue_id)
    .bind(seed.capacity)
    .bind(seed.price_cents)
    .execute(db)
    .await
    .with_context(|| format!("insert time_slot {} {}", seed.date, seed.start_time))?;
    Ok(id)
}

/// Insert a rental booking unless the slot already has one, whoever booked
/// it and in whatever status (the seed assigns at most one booking per slot,
/// and a slot a developer booked in the app before it became past is not
/// booked again — the partial unique index on `(user_id, time_slot_id)`
/// would not stop a second booker). `price_cents` is the slot's price
/// snapshot, per `bookings::occupancy::insert_occupying_booking_tx`.
async fn insert_booking_if_absent(
    db: &PgPool,
    user_id: Uuid,
    time_slot_id: Uuid,
    status: BookingStatus,
    price_cents: i64,
    created_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bookings WHERE time_slot_id = $1)")
            .bind(time_slot_id)
            .fetch_one(db)
            .await
            .context("check existing booking")?;
    if exists {
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO bookings (id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at)
        VALUES ($1, $2, $3, $4, NULL, $5, $6, $6)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(time_slot_id)
    .bind(status)
    .bind(price_cents)
    .bind(created_at)
    .execute(db)
    .await
    .with_context(|| format!("insert booking for slot {time_slot_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::{NaiveDate, TimeZone, Utc};
    use sqlx::PgPool;
    use uuid::Uuid;

    use dream_fly_backend::utils::studio_clock::StudioNow;

    use super::seed_rentals;
    use crate::dataset::{SeedAccount, VenueSeed, insert_venue, upsert_account, venue_id_by_slug};

    /// The run instant every test here starts at: 2026-03-10T03:00Z
    /// (a Tuesday in Asia/Taipei).
    fn at() -> StudioNow {
        StudioNow {
            tz: "Asia/Taipei".parse().expect("valid IANA name"),
            now: Utc.with_ymd_and_hms(2026, 3, 10, 3, 0, 0).unwrap(),
        }
    }

    /// `at()` moved forward `days` days.
    fn later(days: i64) -> StudioNow {
        StudioNow {
            now: at().now + chrono::Duration::days(days),
            ..at()
        }
    }

    /// The smallest rental catalog: two venues (with their prices) and
    /// three members to book them.
    struct Catalog {
        venues: Vec<(Uuid, i64)>,
        members: Vec<Uuid>,
    }

    async fn minimal_catalog(db: &PgPool) -> Catalog {
        let mut venues = Vec::new();
        for (slug, price) in [("zone-a", 80_000), ("zone-b", 100_000)] {
            insert_venue(
                db,
                &VenueSeed {
                    name: "測試場館",
                    slug,
                    description: "",
                    features: &[],
                },
            )
            .await
            .expect("insert venue");
            venues.push((venue_id_by_slug(db, slug).await.expect("venue id"), price));
        }
        let mut members = Vec::new();
        for i in 1..=3 {
            members.push(
                upsert_account(
                    db,
                    &SeedAccount {
                        email: &format!("seed-member-{i:02}@dreamfly.tw"),
                        name: "示範會員",
                        password_hash: "not-a-real-hash",
                        birth_date: Some(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
                        points: 0,
                        joined_at: Utc.with_ymd_and_hms(2025, 1, 1, 4, 0, 0).unwrap(),
                    },
                    at().now,
                )
                .await
                .expect("upsert seed member"),
            );
        }
        Catalog { venues, members }
    }

    async fn seed(db: &PgPool, c: &Catalog, at: StudioNow) -> (usize, usize) {
        seed_rentals(db, at, &c.venues, &c.members)
            .await
            .expect("seed rentals")
    }

    /// `(booking id, slot id, user id, status, slot date)` of every booking.
    async fn bookings(db: &PgPool) -> HashSet<(Uuid, Uuid, Uuid, String, NaiveDate)> {
        sqlx::query_as(
            "SELECT b.id, b.time_slot_id, b.user_id, b.status::TEXT, ts.date \
             FROM bookings b JOIN time_slots ts ON ts.id = b.time_slot_id",
        )
        .fetch_all(db)
        .await
        .expect("load bookings")
        .into_iter()
        .collect()
    }

    async fn count(db: &PgPool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(db)
            .await
            .expect("count rows")
    }

    /// Only slots dated before the run's studio date are booked, at the
    /// slot's price; the booking rule follows the slot's date, so a run two
    /// weeks later keeps every earlier booking as it was and only books the
    /// slots that have since become past.
    #[sqlx::test]
    async fn past_slots_are_booked_by_date(db: PgPool) {
        let c = minimal_catalog(&db).await;
        let today = at().today();

        let (slots, booked) = seed(&db, &c, at()).await;
        // 14 weeks × 3 weekly slots per venue.
        assert_eq!(slots, 14 * 3 * c.venues.len());
        let first = bookings(&db).await;
        assert_eq!(first.len(), booked);
        assert!(!first.is_empty());
        assert!(first.iter().all(|(.., date)| *date < today), "{first:?}");
        let statuses: HashSet<&str> = first.iter().map(|(_, _, _, s, _)| s.as_str()).collect();
        assert_eq!(
            statuses,
            HashSet::from(["completed", "cancelled", "no_show"])
        );

        let off_price: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM bookings b JOIN time_slots ts ON ts.id = b.time_slot_id \
             WHERE b.price_cents <> ts.price_cents",
        )
        .fetch_one(&db)
        .await
        .expect("count bookings off the slot price");
        assert_eq!(off_price, 0);

        let later_today = later(14).today();
        seed(&db, &c, later(14)).await;
        let second = bookings(&db).await;
        assert!(first.is_subset(&second));
        assert!(second.len() > first.len());
        assert!(second.iter().all(|(.., date)| *date < later_today));
        assert!(
            second
                .difference(&first)
                .all(|(.., date)| *date >= today && *date < later_today),
            "a later run booked a slot that was already past"
        );
    }

    /// No slot ever holds more occupying bookings than its capacity — the
    /// dropped `time_slots_booked_bound` CHECK used to guarantee it — after
    /// a run and a run 40 days later.
    #[sqlx::test]
    async fn never_exceeds_capacity(db: PgPool) {
        let c = minimal_catalog(&db).await;
        seed(&db, &c, at()).await;
        seed(&db, &c, later(40)).await;

        assert!(count(&db, "SELECT COUNT(*) FROM occupying_bookings").await > 0);
        let over_capacity: Vec<(Uuid, i32, i64)> = sqlx::query_as(
            "SELECT ts.id, ts.capacity, COUNT(ob.*) FROM time_slots ts \
             JOIN occupying_bookings ob ON ob.time_slot_id = ts.id \
             GROUP BY ts.id, ts.capacity HAVING COUNT(ob.*) > ts.capacity",
        )
        .fetch_all(&db)
        .await
        .expect("load over-capacity slots");
        assert!(
            over_capacity.is_empty(),
            "over-capacity slots: {over_capacity:?}"
        );
    }

    /// A second run at the same instant reports the same slots and bookings
    /// and adds no slot, booking or occupying booking.
    #[sqlx::test]
    async fn same_instant_rerun_adds_nothing(db: PgPool) {
        let c = minimal_catalog(&db).await;
        let first = seed(&db, &c, at()).await;
        let slots = count(&db, "SELECT COUNT(*) FROM time_slots").await;
        let booking_rows = bookings(&db).await;
        let occupying = count(&db, "SELECT COUNT(*) FROM occupying_bookings").await;

        let second = seed(&db, &c, at()).await;

        assert_eq!(second, first);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM time_slots").await, slots);
        assert_eq!(bookings(&db).await, booking_rows);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM occupying_bookings").await,
            occupying
        );
    }

    /// Drift: a seed booking a developer cancelled stays cancelled on a
    /// rerun, and its slot gains no second booking. No app path cancels a
    /// past (completed / no_show) booking, so the cancel is a direct UPDATE,
    /// as a developer would make it.
    #[sqlx::test]
    async fn rerun_keeps_a_cancelled_seed_booking_cancelled(db: PgPool) {
        let c = minimal_catalog(&db).await;
        seed(&db, &c, at()).await;

        let (booking_id, slot_id): (Uuid, Uuid) = sqlx::query_as(
            "UPDATE bookings SET status = 'cancelled'::booking_status \
             WHERE id = (SELECT id FROM bookings \
                         WHERE status = 'completed'::booking_status ORDER BY id LIMIT 1) \
             RETURNING id, time_slot_id",
        )
        .fetch_one(&db)
        .await
        .expect("cancel a seed booking");

        seed(&db, &c, at()).await;
        seed(&db, &c, later(14)).await;

        let on_slot: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, status::TEXT FROM bookings WHERE time_slot_id = $1")
                .bind(slot_id)
                .fetch_all(&db)
                .await
                .expect("load the slot's bookings");
        assert_eq!(on_slot, vec![(booking_id, "cancelled".to_owned())]);
    }

    /// Drift: a developer books every future seed slot in the app for a
    /// member outside the seed's roster; once those slots are past, a rerun
    /// books none of them again, so each keeps the developer's booking alone.
    /// The booking is a direct INSERT of the `confirmed` row the app's
    /// occupancy insert writes (that path is private to `bookings`).
    #[sqlx::test]
    async fn rerun_leaves_a_slot_booked_in_the_app_alone(db: PgPool) {
        let c = minimal_catalog(&db).await;
        seed(&db, &c, at()).await;
        let developer = upsert_account(
            &db,
            &SeedAccount {
                email: "app-member@dreamfly.tw",
                name: "App 會員",
                password_hash: "not-a-real-hash",
                birth_date: None,
                points: 0,
                joined_at: at().now,
            },
            at().now,
        )
        .await
        .expect("upsert app member");
        let app_booked: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "INSERT INTO bookings (id, user_id, time_slot_id, status, note, price_cents, created_at, updated_at) \
             SELECT gen_random_uuid(), $1, id, 'confirmed'::booking_status, NULL, price_cents, $3, $3 \
             FROM time_slots WHERE date >= $2 \
             RETURNING id, time_slot_id",
        )
        .bind(developer)
        .bind(at().today())
        .bind(at().now)
        .fetch_all(&db)
        .await
        .expect("book the future slots in the app");
        assert!(!app_booked.is_empty());

        seed(&db, &c, later(14)).await;

        let all = bookings(&db).await;
        // Without the app bookings, the rerun would have booked some of these
        // slots (see `past_slots_are_booked_by_date`).
        assert!(
            app_booked.iter().any(|(_, slot)| all
                .iter()
                .any(|(_, s, _, _, date)| s == slot && *date < later(14).today())),
            "no app-booked slot became past"
        );
        for (booking_id, slot_id) in &app_booked {
            let on_slot: Vec<Uuid> = all
                .iter()
                .filter(|(_, s, ..)| s == slot_id)
                .map(|(id, ..)| *id)
                .collect();
            assert_eq!(on_slot, vec![*booking_id], "slot {slot_id}");
        }
    }
}
