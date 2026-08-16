//! Shared wall-clock parsing/weekday math for the board's four RTC
//! sample devices ([`crate::devices::pcf8583`], [`crate::devices::ds1307`],
//! [`crate::devices::ds1629`], [`crate::devices::r2025`]) -- each has its
//! own `DateTime` shape (different year width, different weekday
//! convention), but all four accept an initial time from the same
//! `<device>_time` manifest config string, so the parsing and weekday
//! computation live here once instead of once per device.
//!
//! Format is a fixed `YYYY-MM-DD HH:MM:SS`, 24-hour -- deliberately not
//! any of ISO 8601's pickier variants (no timezone, no 'T' separator):
//! this is a config value a person types into a GUI text field or a
//! `.toml` file by hand, not a machine-to-machine interchange format.

/// A parsed, calendar-valid point in time, in whatever wall-clock
/// meaning the config string's author intended (there's no timezone
/// concept anywhere in this board's I2C devices, so none is attached
/// here either).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallClock {
    pub year: u16, // full year, e.g. 2026 -- each device truncates as its own register width requires
    pub month: u8, // 1-12
    pub date: u8,  // 1-31
    pub hour: u8,  // 0-23
    pub minute: u8,
    pub second: u8,
}

impl WallClock {
    /// Day of week, 0 = Sunday .. 6 = Saturday, via Sakamoto's
    /// algorithm. Each device maps this to its own weekday convention
    /// (DS1307/DS1629 count 1-7, R2025/PCF8583 count 0-6 with an
    /// otherwise arbitrary correspondence to weekday names -- see each
    /// device's module docs).
    pub fn weekday_sun0(&self) -> u8 {
        const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut y = i32::from(self.year);
        if self.month < 3 {
            y -= 1;
        }
        let w = (y + y / 4 - y / 100 + y / 400 + T[(self.month - 1) as usize] + i32::from(self.date)) % 7;
        w as u8
    }
}

/// Parses `YYYY-MM-DD HH:MM:SS`. Rejects anything that isn't that exact
/// shape, and any field out of range for a real calendar date (e.g.
/// month 13, February 30th) -- an emulated RTC's initial time isn't the
/// PCF8583's "deliberately wrong time" fault-injection fixture
/// (`pcf8583.rs`'s own tests), so there's no reason to accept nonsense
/// here that a person almost certainly mistyped.
pub fn parse(s: &str) -> Result<WallClock, String> {
    let bytes = s.as_bytes();
    let fixed = b"0000-00-00 00:00:00";
    if bytes.len() != fixed.len() {
        return Err(format!("expected \"YYYY-MM-DD HH:MM:SS\", got {s:?}"));
    }
    for (i, &b) in bytes.iter().enumerate() {
        let expects_digit = fixed[i] == b'0';
        if expects_digit != b.is_ascii_digit() {
            return Err(format!("expected \"YYYY-MM-DD HH:MM:SS\", got {s:?}"));
        }
        if !expects_digit && b != fixed[i] {
            return Err(format!("expected \"YYYY-MM-DD HH:MM:SS\", got {s:?}"));
        }
    }

    let field = |range: std::ops::Range<usize>| -> u32 { s[range].parse().unwrap() };
    let year = field(0..4);
    let month = field(5..7);
    let date = field(8..10);
    let hour = field(11..13);
    let minute = field(14..16);
    let second = field(17..19);

    if !(1..=12).contains(&month) {
        return Err(format!("month {month} out of range 1-12"));
    }
    if hour > 23 {
        return Err(format!("hour {hour} out of range 0-23"));
    }
    if minute > 59 {
        return Err(format!("minute {minute} out of range 0-59"));
    }
    if second > 59 {
        return Err(format!("second {second} out of range 0-59"));
    }
    let days_in_month = days_in_month(year, month);
    if date < 1 || date > days_in_month {
        return Err(format!("date {date} out of range 1-{days_in_month} for {year:04}-{month:02}"));
    }

    Ok(WallClock {
        year: year as u16,
        month: month as u8,
        date: date as u8,
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
    })
}

fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0, // month is validated separately; never read for an invalid month
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_well_formed_timestamp() {
        assert_eq!(
            parse("2026-08-16 14:30:05"),
            Ok(WallClock {
                year: 2026,
                month: 8,
                date: 16,
                hour: 14,
                minute: 30,
                second: 5,
            })
        );
    }

    #[test]
    fn rejects_the_wrong_shape() {
        assert!(parse("2026/08/16 14:30:05").is_err());
        assert!(parse("2026-08-16T14:30:05").is_err());
        assert!(parse("16-08-2026 14:30:05").is_err());
        assert!(parse("").is_err());
        assert!(parse("2026-08-16 14:30").is_err());
    }

    #[test]
    fn rejects_out_of_range_fields() {
        assert!(parse("2026-13-01 00:00:00").is_err(), "month 13");
        assert!(parse("2026-02-30 00:00:00").is_err(), "Feb 30 in a non-leap year");
        assert!(parse("2026-04-31 00:00:00").is_err(), "April 31st doesn't exist");
        assert!(parse("2026-08-16 24:00:00").is_err(), "hour 24");
        assert!(parse("2026-08-16 14:60:00").is_err(), "minute 60");
    }

    #[test]
    fn accepts_february_29th_in_a_leap_year() {
        assert!(parse("2024-02-29 00:00:00").is_ok());
        assert!(parse("2100-02-29 00:00:00").is_err(), "2100 is not a leap year (divisible by 100, not 400)");
        assert!(parse("2000-02-29 00:00:00").is_ok(), "2000 is a leap year (divisible by 400)");
    }

    #[test]
    fn weekday_matches_known_dates() {
        // 2026-08-16 is a Sunday.
        assert_eq!(parse("2026-08-16 00:00:00").unwrap().weekday_sun0(), 0);
        // 2000-01-01 is a Saturday.
        assert_eq!(parse("2000-01-01 00:00:00").unwrap().weekday_sun0(), 6);
        // 1970-01-01 (the Unix epoch) is a Thursday.
        assert_eq!(parse("1970-01-01 00:00:00").unwrap().weekday_sun0(), 4);
    }
}
