//! 工作室時鐘 (Studio Clock) — the wall-clock semantics of contract §3.18
//! 裁決 2: session/slot dates and times are naive values meaning "what the
//! studio's clock shows", and "today" is the studio-local calendar date.
//! Single home for resolving the configured timezone, converting a naive
//! local (date, time) to UTC (refusing DST-ambiguous or nonexistent
//! instants), and the derived questions every scheduling module asks: "has
//! this instant passed?", "what date/month is it at the studio right now?",
//! and — the shape that used to be hand-copied at four call sites — "reject
//! this local (date, time) if it's already started" (`require_not_started`),
//! plus its polarity-mirrored inverse "reject this local (date, time) if it
//! hasn't started yet" (`require_started` — attendance marking requires a
//! session already underway, the opposite of leave/booking's "not yet").
//!
//! `now` and `tz` are always parameters (never `Utc::now()` internally) so
//! day-boundary and DST edge cases are unit-testable with fixed instants.
//!
//! Two layers of ambiguity handling live here. `to_utc`/`has_started`/
//! `has_ended` return `None` on a DST-ambiguous or nonexistent local time
//! and leave the mapping to an error up to the caller — for sites where the
//! result isn't a straight reject (e.g. `sessions::model`'s display-status
//! derivation). `to_utc_checked` and `require_not_started` are the
//! `AppError`-carrying wrappers layered on top for the common "reject an
//! already-started local (date, time)" gate: `to_utc_checked` maps
//! ambiguity to `BadRequest("{noun} falls on an ambiguous local time")`,
//! and `require_not_started` adds the "already started" check on top,
//! taking that error whole from the caller since each of the four sites
//! uses a different `AppError` variant and wording (422 `Validation` 中文
//! ×2, 400 `BadRequest` 英文 ×2) — the same "caller owns the wording" shape
//! as `AuthUser::owns_or_admin`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::config::ServerConfig;
use crate::error::AppError;

/// Resolve the studio timezone. Falls back to UTC with a warning so a
/// misconfigured deploy still runs, just without correct local-time rules.
/// Startup validation (`AppConfig::load`) already rejects invalid timezone
/// names, so the fallback only fires if a future refactor bypasses that.
pub fn studio_tz(server: &ServerConfig) -> Tz {
    server.studio_timezone.parse::<Tz>().unwrap_or_else(|_| {
        tracing::warn!(
            tz = %server.studio_timezone,
            "invalid studio_timezone; falling back to UTC"
        );
        chrono_tz::UTC
    })
}

/// The studio-local calendar date of a UTC instant — "today" per contract
/// §3.18 裁決 2: at 23:00 UTC the studio (Asia/Taipei, UTC+8) is already
/// 07:00 the *next* day, and a coach checking their morning
/// `GET /sessions/today` must get that next day, not UTC's date.
pub fn today(tz: Tz, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&tz).date_naive()
}

/// The studio-local `YYYY-MM` month key of a UTC instant — the Rust twin of
/// the SQL `to_char(..., 'YYYY-MM')` used throughout `reports::repository`.
/// Same zone-then-format order as [`today`]: a UTC instant just after a
/// studio-local month boundary must key to the studio's month, not UTC's.
pub fn month_key(tz: Tz, now: DateTime<Utc>) -> String {
    now.with_timezone(&tz).format("%Y-%m").to_string()
}

/// Convert a naive studio-local (date, time) to the UTC instant it names.
/// `None` when the local time is DST-ambiguous or nonexistent — callers
/// treat that as invalid input rather than picking an interpretation
/// arbitrarily.
pub fn to_utc(tz: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&NaiveDateTime::new(date, time))
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Like [`to_utc`], but maps a DST-ambiguous/nonexistent local time to the
/// `BadRequest` text every call site used to hand-roll identically:
/// `"{noun} falls on an ambiguous local time"`. `noun` names the local
/// value being converted (e.g. `"time slot"`, `"session time"`) so the
/// message reads naturally at each call site.
pub fn to_utc_checked(
    tz: Tz,
    date: NaiveDate,
    time: NaiveTime,
    noun: &str,
) -> Result<DateTime<Utc>, AppError> {
    to_utc(tz, date, time)
        .ok_or_else(|| AppError::BadRequest(format!("{noun} falls on an ambiguous local time")))
}

/// Reject a studio-local (date, time) that has already started — its UTC
/// instant is at or before `now` (`<=`, the same inclusive boundary as
/// [`has_started`]) — returning `started` verbatim when it has. Every call
/// site uses a different `AppError` variant and wording for "already
/// started" (422 `Validation` 中文 ×2, 400 `BadRequest` 英文 ×2), so the
/// caller supplies that error whole rather than this function picking a
/// fixed variant (same "caller owns the wording" shape as
/// `AuthUser::owns_or_admin`). `noun` only feeds the ambiguous-time error
/// via [`to_utc_checked`].
pub fn require_not_started(
    tz: Tz,
    now: DateTime<Utc>,
    date: NaiveDate,
    time: NaiveTime,
    noun: &str,
    started: AppError,
) -> Result<(), AppError> {
    if to_utc_checked(tz, date, time, noun)? <= now {
        return Err(started);
    }
    Ok(())
}

/// Reject a studio-local (date, time) that hasn't started yet — its UTC
/// instant is after `now` (`>`) — returning `not_started` verbatim when it
/// hasn't. The polarity-mirrored inverse of [`require_not_started`]: its
/// strict complement (`>` here vs. `<=` there), so the start instant itself
/// is already allowed — the same inclusive boundary as [`has_started`],
/// just approached from the other side (attendance marking becomes allowed
/// the moment a session starts, rather than becoming disallowed).
///
/// DST ambiguity is still a `BadRequest` via [`to_utc_checked`], not a
/// `not_started` — but note an asymmetry with [`require_not_started`]'s
/// ambiguous case: there the *caller* picks the local time being checked
/// (a new leave/booking request), so they can simply resubmit against a
/// different instant; here the local time is the *session's own fixed
/// schedule*, so an ambiguous session start would permanently 400 every
/// attendance-marking attempt for that session, with no retry able to
/// change the outcome. Purely theoretical under `Asia/Taipei` (no DST) —
/// documented here, not special-cased.
pub fn require_started(
    tz: Tz,
    now: DateTime<Utc>,
    date: NaiveDate,
    time: NaiveTime,
    noun: &str,
    not_started: AppError,
) -> Result<(), AppError> {
    if to_utc_checked(tz, date, time, noun)? > now {
        return Err(not_started);
    }
    Ok(())
}

/// Whether a studio-local (date, time) is at or before `now`. `None` on a
/// DST-ambiguous/nonexistent local time (see `to_utc`).
pub fn has_started(tz: Tz, now: DateTime<Utc>, date: NaiveDate, time: NaiveTime) -> Option<bool> {
    to_utc(tz, date, time).map(|utc| utc <= now)
}

/// Whether a studio-local (date, time) is at or before `now`. `None` on a
/// DST-ambiguous/nonexistent local time (see `to_utc`). Symmetric with
/// [`has_started`] — same formula, applied to an end instant instead of a
/// start instant.
pub fn has_ended(tz: Tz, now: DateTime<Utc>, date: NaiveDate, time: NaiveTime) -> Option<bool> {
    to_utc(tz, date, time).map(|utc| utc <= now)
}

/// `(first_day, last_day)` of the given calendar month — the single owner
/// of the "roll to the first of next month, then step back one day" dance
/// that `reports::service::studio_month_bounds` and
/// `schedule::repository::find_by_month` used to hand-roll themselves.
/// `None` on an invalid `month` (must be `1..=12`) or when the December
/// rollover's `year + 1` would overflow — `from_ymd_opt`'s year-range
/// check rejects the year first, so `i32::MAX` still can't panic;
/// `checked_add` here is defensive redundancy, not the actual guard.
pub fn month_bounds(year: i32, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first_day = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year.checked_add(1)?, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }?;
    let last_day = next_month_first.pred_opt()?;
    Some((first_day, last_day))
}

/// Parse a `YYYY-MM-DD` calendar date string — the single owner of that
/// format knowledge, replacing three call sites that used to hand-roll
/// `NaiveDate::parse_from_str(s, "%Y-%m-%d")` themselves. Mirrors
/// [`parse_time_of_day`]'s `Option` convention: each call site keeps its
/// own 400 wording (they differ — with/without the input value echoed
/// back, different phrasing) via `.ok_or_else(...)`, the same "caller owns
/// the wording" shape as [`to_utc_checked`]/[`require_not_started`].
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// Parse an `HH:MM` or `HH:MM:SS` time-of-day string. Accepting both formats
/// makes the API lenient to callers that send whatever their UI produces
/// (HTML `<input type="time">` for example sometimes emits seconds), without
/// forcing clients to strip trailing `:00`.
pub fn parse_time_of_day(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .ok()
}

/// Single owner of the "end ≤ start" rejection shared by the three
/// schedule-shaped call sites (courses/coaches/schedule); rejects with
/// `Validation`/422, the same boundary as all four tables' own `CHECK (end_time > start_time)` backstop.
pub fn validate_time_window(start: NaiveTime, end: NaiveTime) -> Result<(), AppError> {
    if end <= start {
        return Err(AppError::Validation(
            "end_time must be after start_time".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taipei() -> Tz {
        "Asia/Taipei".parse::<Tz>().expect("valid IANA name")
    }

    fn new_york() -> Tz {
        "America/New_York".parse::<Tz>().expect("valid IANA name")
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn server(tz: &str) -> ServerConfig {
        ServerConfig {
            host: "0.0.0.0".into(),
            port: 3000,
            allowed_origins: vec![],
            trust_proxy: false,
            studio_timezone: tz.into(),
        }
    }

    // --- studio_tz ---

    #[test]
    fn studio_tz_parses_configured_zone() {
        assert_eq!(studio_tz(&server("Asia/Taipei")), taipei());
    }

    #[test]
    fn studio_tz_falls_back_to_utc_on_invalid_name() {
        assert_eq!(studio_tz(&server("Not/AZone")), chrono_tz::UTC);
    }

    // --- today (ported from sessions::service::studio_date_at tests) ---

    #[test]
    fn today_before_taipei_midnight_matches_utc_date() {
        // 15:59:59Z = 23:59:59 Taipei — still the same calendar day in
        // both zones.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 15, 59, 59).unwrap();
        assert_eq!(today(taipei(), now), d(2026, 7, 5));
    }

    #[test]
    fn today_at_taipei_midnight_rolls_to_next_day() {
        // 16:00:00Z = 00:00:00 Taipei of the NEXT day — Taipei's date must
        // win over UTC's (which is still July 5).
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 16, 0, 0).unwrap();
        assert_eq!(today(taipei(), now), d(2026, 7, 6));
    }

    #[test]
    fn today_taipei_early_morning_is_next_utc_day() {
        // 22:00:00Z = 06:00 Taipei next day — the "coach checks morning
        // sessions at 6-7am Taipei" scenario (contract §3.18 裁決 2's own
        // example) this helper exists for.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 22, 0, 0).unwrap();
        assert_eq!(today(taipei(), now), d(2026, 7, 6));
    }

    #[test]
    fn today_under_utc_config_is_plain_utc_date() {
        // The integration-test harness pins studio_timezone to UTC — under
        // that config the helper must degrade to the plain UTC date.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 22, 0, 0).unwrap();
        assert_eq!(today(chrono_tz::UTC, now), d(2026, 7, 5));
    }

    // --- month_key ---

    #[test]
    fn month_key_rolls_to_next_month_at_taipei_midnight() {
        // 16:00:00Z on the 30th = 00:00:00 Taipei on the 1st of the NEXT
        // month — the studio's month must win over UTC's (still June).
        let now = Utc.with_ymd_and_hms(2026, 6, 30, 16, 0, 0).unwrap();
        assert_eq!(month_key(taipei(), now), "2026-07");
    }

    #[test]
    fn month_key_stays_in_previous_month_when_studio_tz_is_behind_utc() {
        // 02:00:00Z on Aug 1st = 22:00:00 New York (EDT, UTC-4) on Jul 31st
        // — the studio's month must stay July even though UTC has already
        // rolled into August. Also exercises zero-padding ("07", not "7").
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 2, 0, 0).unwrap();
        assert_eq!(month_key(new_york(), now), "2026-07");
    }

    // --- has_started (ported from leave::service::session_has_started tests) ---

    #[test]
    fn has_started_false_when_now_is_before_start() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 8, 0, 0).unwrap();
        assert_eq!(
            has_started(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)),
            Some(false)
        );
    }

    #[test]
    fn has_started_true_when_now_is_at_or_after_start() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        assert_eq!(
            has_started(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)),
            Some(true)
        );

        let later = Utc.with_ymd_and_hms(2026, 7, 5, 9, 30, 0).unwrap();
        assert_eq!(
            has_started(chrono_tz::UTC, later, d(2026, 7, 5), t(9, 0)),
            Some(true)
        );
    }

    #[test]
    fn has_started_uses_studio_local_wall_clock_not_utc_date() {
        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th (UTC+8). A session
        // dated the 6th at 08:00 Taipei-local has NOT started yet at that
        // instant, even though the UTC calendar date is still the 5th.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 23, 30, 0).unwrap();
        assert_eq!(has_started(taipei(), now, d(2026, 7, 6), t(8, 0)), Some(false));

        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th — a session dated
        // the 6th at 07:00 Taipei-local HAS already started.
        assert_eq!(has_started(taipei(), now, d(2026, 7, 6), t(7, 0)), Some(true));
    }

    // --- has_ended (mirrors has_started) ---

    #[test]
    fn has_ended_false_when_now_is_before_end() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 8, 0, 0).unwrap();
        assert_eq!(
            has_ended(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)),
            Some(false)
        );
    }

    #[test]
    fn has_ended_true_when_now_is_at_or_after_end() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        assert_eq!(
            has_ended(chrono_tz::UTC, now, d(2026, 7, 5), t(9, 0)),
            Some(true)
        );

        let later = Utc.with_ymd_and_hms(2026, 7, 5, 9, 30, 0).unwrap();
        assert_eq!(
            has_ended(chrono_tz::UTC, later, d(2026, 7, 5), t(9, 0)),
            Some(true)
        );
    }

    #[test]
    fn has_ended_none_on_new_york_dst_ambiguous_time() {
        // Nonexistent local time (spring forward) — mirrors
        // to_utc_none_on_nonexistent_spring_forward_time.
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 12, 0, 0).unwrap();
        assert_eq!(has_ended(new_york(), now, d(2026, 3, 8), t(2, 30)), None);

        // Ambiguous local time (fall back) — mirrors
        // to_utc_none_on_ambiguous_fall_back_time.
        let now2 = Utc.with_ymd_and_hms(2026, 11, 1, 12, 0, 0).unwrap();
        assert_eq!(has_ended(new_york(), now2, d(2026, 11, 1), t(1, 30)), None);
    }

    #[test]
    fn has_ended_uses_studio_local_wall_clock_not_utc_date() {
        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th (UTC+8). A session
        // dated the 6th and ending at 08:00 Taipei-local has NOT ended yet
        // at that instant, even though the UTC calendar date is still the
        // 5th.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 23, 30, 0).unwrap();
        assert_eq!(has_ended(taipei(), now, d(2026, 7, 6), t(8, 0)), Some(false));

        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th — a session dated
        // the 6th and ending at 07:00 Taipei-local HAS already ended.
        assert_eq!(has_ended(taipei(), now, d(2026, 7, 6), t(7, 0)), Some(true));
    }

    // --- to_utc DST edges (Taipei has no DST; use America/New_York) ---

    #[test]
    fn to_utc_none_on_nonexistent_spring_forward_time() {
        // 2026-03-08 02:30 America/New_York does not exist (clocks jump
        // 02:00 → 03:00).
        assert_eq!(to_utc(new_york(), d(2026, 3, 8), t(2, 30)), None);
    }

    #[test]
    fn to_utc_none_on_ambiguous_fall_back_time() {
        // 2026-11-01 01:30 America/New_York occurs twice (clocks fall back
        // 02:00 → 01:00).
        assert_eq!(to_utc(new_york(), d(2026, 11, 1), t(1, 30)), None);
    }

    #[test]
    fn to_utc_converts_unambiguous_local_time() {
        // 09:00 Taipei = 01:00 UTC same day, year-round (no DST).
        let expected = Utc.with_ymd_and_hms(2026, 7, 6, 1, 0, 0).unwrap();
        assert_eq!(to_utc(taipei(), d(2026, 7, 6), t(9, 0)), Some(expected));
    }

    // --- to_utc_checked / require_not_started ---

    #[test]
    fn to_utc_checked_converts_unambiguous_local_time() {
        // Mirrors `to_utc_converts_unambiguous_local_time`: 09:00 Taipei =
        // 01:00 UTC same day, year-round (no DST).
        let expected = Utc.with_ymd_and_hms(2026, 7, 6, 1, 0, 0).unwrap();
        assert_eq!(
            to_utc_checked(taipei(), d(2026, 7, 6), t(9, 0), "session time").unwrap(),
            expected
        );
    }

    #[test]
    fn to_utc_checked_embeds_noun_in_ambiguous_message() {
        // Nonexistent local time (spring forward) — mirrors
        // `to_utc_none_on_nonexistent_spring_forward_time`.
        let err = to_utc_checked(new_york(), d(2026, 3, 8), t(2, 30), "time slot").unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "time slot falls on an ambiguous local time")
        );
    }

    #[test]
    fn require_not_started_returns_started_error_verbatim_when_already_started() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 9, 30, 0).unwrap();
        let err = require_not_started(
            chrono_tz::UTC,
            now,
            d(2026, 7, 5),
            t(9, 0),
            "session time",
            AppError::Validation("場次已開始，無法請假".into()),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(ref m) if m == "場次已開始，無法請假"));
    }

    #[test]
    fn require_not_started_blocks_at_exact_boundary_but_not_before() {
        // `<=`: `now` exactly equal to the start instant already blocks —
        // mirrors `has_started_true_when_now_is_at_or_after_start`'s
        // boundary.
        let start = d(2026, 7, 5);
        let at_start = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        let err = require_not_started(
            chrono_tz::UTC,
            at_start,
            start,
            t(9, 0),
            "time slot",
            AppError::BadRequest("cannot book a time slot that has already started".into()),
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "cannot book a time slot that has already started")
        );

        let before_start = Utc.with_ymd_and_hms(2026, 7, 5, 8, 59, 59).unwrap();
        assert!(
            require_not_started(
                chrono_tz::UTC,
                before_start,
                start,
                t(9, 0),
                "time slot",
                AppError::BadRequest("cannot book a time slot that has already started".into()),
            )
            .is_ok()
        );
    }

    // --- require_started (polarity-mirrored inverse of require_not_started) ---

    #[test]
    fn require_started_returns_not_started_error_verbatim_when_not_yet_started() {
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 8, 0, 0).unwrap();
        let err = require_started(
            chrono_tz::UTC,
            now,
            d(2026, 7, 5),
            t(9, 0),
            "session time",
            AppError::Validation("場次尚未開始，無法點名".into()),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(ref m) if m == "場次尚未開始，無法點名"));
    }

    #[test]
    fn require_started_allows_at_exact_boundary_but_not_before() {
        // `>`: `now` exactly equal to the start instant is already
        // allowed — `require_not_started`'s strict complement, and mirrors
        // `has_started_true_when_now_is_at_or_after_start`'s boundary.
        let start = d(2026, 7, 5);
        let at_start = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
        assert!(
            require_started(
                chrono_tz::UTC,
                at_start,
                start,
                t(9, 0),
                "session time",
                AppError::Validation("not started".into()),
            )
            .is_ok()
        );

        let before_start = Utc.with_ymd_and_hms(2026, 7, 5, 8, 59, 59).unwrap();
        let err = require_started(
            chrono_tz::UTC,
            before_start,
            start,
            t(9, 0),
            "session time",
            AppError::Validation("not started".into()),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(ref m) if m == "not started"));
    }

    #[test]
    fn require_started_ambiguous_local_time_returns_bad_request() {
        // Nonexistent local time (spring forward) — mirrors
        // `to_utc_none_on_nonexistent_spring_forward_time`. DST ambiguity is
        // still a 400 under this inverse gate too — see the function doc's
        // note on why that's a *permanent* block for a session's fixed
        // schedule (unlike `require_not_started`, where the caller picks
        // the local time and can simply avoid the ambiguous instant).
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 12, 0, 0).unwrap();
        let err = require_started(
            new_york(),
            now,
            d(2026, 3, 8),
            t(2, 30),
            "session time",
            AppError::Validation("not started".into()),
        )
        .unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m == "session time falls on an ambiguous local time")
        );
    }

    #[test]
    fn require_started_uses_studio_local_wall_clock_not_utc_date() {
        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th (UTC+8). A session
        // dated the 6th at 08:00 Taipei-local has NOT started yet at that
        // instant, even though the UTC calendar date is still the 5th.
        let now = Utc.with_ymd_and_hms(2026, 7, 5, 23, 30, 0).unwrap();
        let err = require_started(
            taipei(),
            now,
            d(2026, 7, 6),
            t(8, 0),
            "session time",
            AppError::Validation("not started".into()),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(ref m) if m == "not started"));

        // 23:30 UTC on the 5th = 07:30 Taipei on the 6th — a session dated
        // the 6th at 07:00 Taipei-local HAS already started.
        assert!(
            require_started(
                taipei(),
                now,
                d(2026, 7, 6),
                t(7, 0),
                "session time",
                AppError::Validation("not started".into()),
            )
            .is_ok()
        );
    }

    // --- month_bounds ---

    #[test]
    fn month_bounds_returns_first_and_last_day_of_month() {
        assert_eq!(month_bounds(2026, 7), Some((d(2026, 7, 1), d(2026, 7, 31))));
    }

    #[test]
    fn month_bounds_handles_december_year_rollover() {
        // Exercises the `month == 12` branch: next-month-first is Jan 1 of
        // `year + 1`, stepped back one day to land on Dec 31 of `year`.
        assert_eq!(month_bounds(2026, 12), Some((d(2026, 12, 1), d(2026, 12, 31))));
    }

    #[test]
    fn month_bounds_none_on_month_zero() {
        assert_eq!(month_bounds(2026, 0), None);
    }

    #[test]
    fn month_bounds_none_on_month_thirteen() {
        assert_eq!(month_bounds(2026, 13), None);
    }

    #[test]
    fn month_bounds_none_on_i32_max_year_does_not_panic() {
        // `from_ymd_opt`'s year-range check rejects `i32::MAX` before the
        // December rollover's `year + 1` is ever reached; `checked_add`
        // guarding that arithmetic is defensive redundancy here, not what
        // actually stops the panic.
        assert_eq!(month_bounds(i32::MAX, 12), None);
    }

    // --- parse_date ---

    #[test]
    fn parse_date_accepts_iso_format() {
        assert_eq!(parse_date("2026-07-05"), Some(d(2026, 7, 5)));
    }

    #[test]
    fn parse_date_rejects_garbage_format() {
        assert_eq!(parse_date("07/05/2026"), None);
        assert_eq!(parse_date("not-a-date"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn parse_date_rejects_invalid_calendar_date() {
        // February never has 30 days, even in a leap year.
        assert_eq!(parse_date("2026-02-30"), None);
    }

    // --- parse_time_of_day ---

    #[test]
    fn parse_time_of_day_accepts_both_formats() {
        assert_eq!(parse_time_of_day("09:00"), Some(t(9, 0)));
        assert_eq!(
            parse_time_of_day("09:00:30"),
            NaiveTime::from_hms_opt(9, 0, 30)
        );
    }

    #[test]
    fn parse_time_of_day_rejects_garbage() {
        assert_eq!(parse_time_of_day("9am"), None);
        assert_eq!(parse_time_of_day("25:00"), None);
        assert_eq!(parse_time_of_day(""), None);
    }

    // --- validate_time_window ---

    #[test]
    fn validate_time_window_ok_when_end_after_start() {
        assert!(validate_time_window(t(9, 0), t(10, 0)).is_ok());
    }

    #[test]
    fn validate_time_window_rejects_end_equal_start() {
        let err = validate_time_window(t(9, 0), t(9, 0)).unwrap_err();
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "end_time must be after start_time")
        );
    }

    #[test]
    fn validate_time_window_rejects_end_before_start() {
        let err = validate_time_window(t(10, 0), t(9, 0)).unwrap_err();
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "end_time must be after start_time")
        );
    }
}
