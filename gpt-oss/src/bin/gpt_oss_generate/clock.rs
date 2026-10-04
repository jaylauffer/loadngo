//! The local date and time the chat tells the model.

/// The date and time the chat tells the model.
pub struct Now {
    /// `YYYY-MM-DD`, local: the system message's "Current date".
    pub date: String,
    /// A sentence with the weekday, date, time and offset from UTC.
    pub said: String,
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `(year, month 1-12, day)` of a count of days since 1970-01-01 (Howard Hinnant's
/// days-to-civil).
fn civil(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month as usize, day)
}

/// Now, in the machine's time zone. Until 2026-10-04 this was UTC: at 06:07 on Sunday 4
/// October at +07 the model was told 2026-10-03, and answered "Wednesday, October 3".
pub fn now() -> Now {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    // Seconds east of UTC for the local zone (0, UTC, where it cannot be read).
    let offset = local_offset(seconds);
    let local = seconds + offset;
    let days = local.div_euclid(86_400);
    let (year, month, day) = civil(days);
    let weekday = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
    let minute = local.rem_euclid(86_400) / 60;
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
        date: format!("{year:04}-{month:02}-{day:02}"),
        said: format!(
            "It is now {weekday}, {day} {} {year}, {:02}:{:02} local time ({zone}); the \
             current date above is local too. Answer questions about the date or time from \
             this; do not look them up.",
            MONTHS[month - 1],
            minute / 60,
            minute % 60
        ),
    }
}

#[cfg(unix)]
// tm_gmtoff is a C long: 64 bits here, 32 on 32-bit targets.
#[allow(clippy::useless_conversion)]
fn local_offset(seconds: i64) -> i64 {
    let time = seconds as libc::time_t;
    // SAFETY: localtime_r writes only into the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let filled = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if filled {
        i64::from(tm.tm_gmtoff)
    } else {
        0
    }
}

#[cfg(not(unix))]
fn local_offset(_seconds: i64) -> i64 {
    0
}
