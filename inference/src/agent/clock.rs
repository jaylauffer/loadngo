//! The local date and time a chat tells its model each turn.
//!
//! Until 2026-10-04 gpt-oss's chat gave the date in UTC: at 06:07 on Sunday 4 October
//! at +07 the model was told 2026-10-03, searched the web seven times and answered
//! "Wednesday, October 3". The date is local now, with the weekday, time and offset
//! said outright, and the model is told not to look them up.

use chrono::{DateTime, FixedOffset, Local};

/// The date and time the chat tells the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Now {
    /// `YYYY-MM-DD`, local.
    pub date: String,
    /// A sentence with the weekday, date, time and offset from UTC.
    pub said: String,
}

/// Now, in this machine's time zone.
#[must_use]
pub fn now() -> Now {
    at(Local::now().fixed_offset())
}

/// What the chat says at `time`.
#[must_use]
pub fn at(time: DateTime<FixedOffset>) -> Now {
    let offset = time.offset().local_minus_utc();
    let zone = if offset == 0 {
        "UTC".to_owned()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "UTC{sign}{:02}:{:02}",
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        )
    };
    Now {
        date: time.format("%Y-%m-%d").to_string(),
        said: format!(
            "It is now {}, {} local time ({zone}). Answer questions about the date or time \
             from this; do not look them up.",
            time.format("%A, %-d %B %Y"),
            time.format("%H:%M"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weekday_and_date_are_local() {
        // 06:07 +07 on Sunday 4 October 2026 is still Saturday 3 October in UTC.
        let time = DateTime::parse_from_rfc3339("2026-10-04T06:07:00+07:00").unwrap();
        let now = at(time);
        assert_eq!(now.date, "2026-10-04");
        assert!(
            now.said
                .starts_with("It is now Sunday, 4 October 2026, 06:07 local time (UTC+07:00)."),
            "{}",
            now.said
        );
        let utc = at(DateTime::parse_from_rfc3339("2026-10-03T23:07:00+00:00").unwrap());
        assert_eq!(utc.date, "2026-10-03");
        assert!(utc
            .said
            .contains("Saturday, 3 October 2026, 23:07 local time (UTC)"));
        let west = at(DateTime::parse_from_rfc3339("2026-01-05T09:30:00-03:30").unwrap());
        assert!(west.said.contains("(UTC-03:30)"));
    }
}
