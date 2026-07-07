//! 工作室時鐘 (Studio Clock) — the wall-clock semantics of contract §3.18
//! 裁決 2: session/slot dates and times are naive values meaning "what the
//! studio's clock shows", and "today" is the studio-local calendar date.
//! Single home for resolving the configured timezone, converting a naive
//! local (date, time) to UTC (refusing DST-ambiguous or nonexistent
//! instants), and the two derived questions every scheduling module asks:
//! "has this instant passed?" and "what date is it at the studio right now?".
//!
//! `now` and `tz` are always parameters (never `Utc::now()` internally) so
//! day-boundary and DST edge cases are unit-testable with fixed instants.
//! Ambiguity yields `None`; each caller maps that to its own
//! endpoint-specific error message, so API error text stays per-endpoint.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::config::ServerConfig;

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

/// Convert a naive studio-local (date, time) to the UTC instant it names.
/// `None` when the local time is DST-ambiguous or nonexistent — callers
/// treat that as invalid input rather than picking an interpretation
/// arbitrarily.
pub fn to_utc(tz: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&NaiveDateTime::new(date, time))
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Whether a studio-local (date, time) is at or before `now`. `None` on a
/// DST-ambiguous/nonexistent local time (see `to_utc`).
pub fn has_started(tz: Tz, now: DateTime<Utc>, date: NaiveDate, time: NaiveTime) -> Option<bool> {
    to_utc(tz, date, time).map(|utc| utc <= now)
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
}
