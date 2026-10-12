//! Plan times. The C++ Task, and the Win32 port after it, store time entries
//! as local wall-clock time in 100 ns ticks since 1601-01-01 (a local
//! FILETIME), so a plan saved on Windows opens here with the same times.

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use data::model_utils::UNITS_PER_HOUR;

/// Ticks in one 15-minute slot, the planner's unit.
pub const TICKS_PER_SLOT: u64 = UNITS_PER_HOUR / 4;
/// Slots in a day.
pub const SLOTS_PER_DAY: u64 = 24 * 4;
/// Ticks in one day.
pub const TICKS_PER_DAY: u64 = UNITS_PER_HOUR * 24;

const TICKS_PER_SECOND: i64 = 10_000_000;

fn epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1601, 1, 1)
        .expect("valid date")
        .and_time(NaiveTime::MIN)
}

/// Ticks for a local date and time.
pub fn ticks_from_local(when: NaiveDateTime) -> u64 {
    let since = when - epoch();
    let seconds = since.num_seconds();
    let below = since - TimeDelta::seconds(seconds);
    let ticks = seconds * TICKS_PER_SECOND + i64::from(below.subsec_nanos()) / 100;
    u64::try_from(ticks).unwrap_or(0)
}

/// The local date and time of `ticks`.
pub fn local_from_ticks(ticks: u64) -> NaiveDateTime {
    let seconds = (ticks / TICKS_PER_SECOND as u64) as i64;
    let nanos = (ticks % TICKS_PER_SECOND as u64) * 100;
    epoch() + TimeDelta::seconds(seconds) + TimeDelta::nanoseconds(nanos as i64)
}

/// Ticks at local midnight starting `date`.
pub fn day_start(date: NaiveDate) -> u64 {
    ticks_from_local(date.and_time(NaiveTime::MIN))
}

/// Today's local date.
pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// "Monday, October 13, 2026", as the Win32 banner shows a date.
pub fn banner_text(date: NaiveDate) -> String {
    date.format("%A, %B %-d, %Y").to_string()
}

/// "9am", "12pm": the planner's hour labels.
pub fn hour_label(hour: u32) -> String {
    let suffix = if hour < 12 { "am" } else { "pm" };
    let shown = match hour % 12 {
        0 => 12,
        other => other,
    };
    format!("{shown}{suffix}")
}

/// "9:15am", the start of a slot in a day.
pub fn slot_label(slot: u64) -> String {
    let minutes = slot * 15;
    let hour = (minutes / 60) as u32;
    let label = hour_label(hour);
    let (number, suffix) = label.split_at(label.len() - 2);
    format!("{number}:{:02}{suffix}", minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_round_trip_through_local_time() {
        let when = NaiveDate::from_ymd_opt(2026, 10, 13)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap();
        let ticks = ticks_from_local(when);
        assert_eq!(local_from_ticks(ticks), when);
        assert_eq!(
            ticks - day_start(when.date()),
            9 * UNITS_PER_HOUR + TICKS_PER_SLOT
        );
    }

    #[test]
    fn epoch_matches_filetime() {
        // 1970-01-01 is 11,644,473,600 seconds after 1601-01-01 in FILETIME.
        let unix = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        assert_eq!(day_start(unix), 11_644_473_600 * 10_000_000);
    }

    #[test]
    fn labels() {
        assert_eq!(hour_label(0), "12am");
        assert_eq!(hour_label(9), "9am");
        assert_eq!(hour_label(12), "12pm");
        assert_eq!(hour_label(23), "11pm");
        assert_eq!(slot_label(37), "9:15am");
        assert_eq!(slot_label(52), "1:00pm");
        let date = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        assert_eq!(banner_text(date), "Tuesday, October 13, 2026");
    }
}
