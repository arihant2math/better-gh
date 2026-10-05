//! POSIX 5-field cron expressions (`minute hour day-of-month month
//! day-of-week`), evaluated in UTC, as used by `on.schedule`.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use super::WorkflowError;

const MONTH_NAMES: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const DAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// How far [`CronSchedule::next_after`] searches, in days (about 5 years:
/// enough for `0 0 29 2 *` across a normal 4-year leap-day gap).
const SEARCH_DAYS: i64 = 366 * 5 + 2;

/// A parsed cron schedule. Each field is a bit set of allowed values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronSchedule {
    /// The original expression.
    pub source: String,
    minutes: u64,
    hours: u32,
    /// bits 1..=31
    days_of_month: u32,
    /// bits 1..=12
    months: u16,
    /// bits 0..=6 (0 = Sunday)
    days_of_week: u8,
    /// The day-of-month field starts with `*` (unrestricted).
    dom_star: bool,
    /// The day-of-week field starts with `*` (unrestricted).
    dow_star: bool,
}

#[derive(Clone, Copy)]
enum Field {
    Minute,
    Hour,
    DayOfMonth,
    Month,
    DayOfWeek,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Minute => "minute",
            Field::Hour => "hour",
            Field::DayOfMonth => "day of month",
            Field::Month => "month",
            Field::DayOfWeek => "day of week",
        }
    }

    /// Inclusive bounds accepted in the expression (dow accepts 7 = Sunday).
    fn bounds(self) -> (u32, u32) {
        match self {
            Field::Minute => (0, 59),
            Field::Hour => (0, 23),
            Field::DayOfMonth => (1, 31),
            Field::Month => (1, 12),
            Field::DayOfWeek => (0, 7),
        }
    }
}

fn err(expr: &str, msg: impl std::fmt::Display) -> WorkflowError {
    WorkflowError::invalid(format!("invalid cron expression '{expr}': {msg}"))
}

fn parse_value(field: Field, s: &str, expr: &str) -> Result<u32, WorkflowError> {
    let upper = s.to_ascii_uppercase();
    let named = match field {
        Field::Month => MONTH_NAMES
            .iter()
            .position(|n| *n == upper)
            .map(|i| i as u32 + 1),
        Field::DayOfWeek => DAY_NAMES.iter().position(|n| *n == upper).map(|i| i as u32),
        _ => None,
    };
    if let Some(v) = named {
        return Ok(v);
    }
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(expr, format!("invalid {} value '{s}'", field.name())));
    }
    let v: u32 = s
        .parse()
        .map_err(|_| err(expr, format!("invalid {} value '{s}'", field.name())))?;
    let (lo, hi) = field.bounds();
    if v < lo || v > hi {
        return Err(err(
            expr,
            format!("{} value {v} out of range {lo}-{hi}", field.name()),
        ));
    }
    Ok(v)
}

/// Parses one field into a bit set (bit n = value n allowed).
fn parse_field(field: Field, text: &str, expr: &str) -> Result<u64, WorkflowError> {
    let (lo, hi) = field.bounds();
    let hi = if matches!(field, Field::DayOfWeek) {
        6
    } else {
        hi
    };
    let mut bits = 0u64;
    for item in text.split(',') {
        if item.is_empty() {
            return Err(err(
                expr,
                format!("empty list item in {} field", field.name()),
            ));
        }
        let (base, step) = match item.split_once('/') {
            Some((b, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| err(expr, format!("invalid step '{s}'")))?;
                if step == 0 {
                    return Err(err(expr, "step must be greater than 0"));
                }
                (b, Some(step))
            }
            None => (item, None),
        };
        let (start, end) = if base == "*" {
            (lo, hi)
        } else if let Some((a, b)) = base.split_once('-') {
            let a = parse_value(field, a, expr)?;
            let b = parse_value(field, b, expr)?;
            if a > b {
                return Err(err(expr, format!("invalid range '{base}'")));
            }
            (a, b)
        } else {
            let a = parse_value(field, base, expr)?;
            // `5/10` means "from 5 to the max, every 10".
            let end = if step.is_some() { field.bounds().1 } else { a };
            (a, end)
        };
        let step = step.unwrap_or(1);
        let mut v = start;
        while v <= end {
            let normalized = if matches!(field, Field::DayOfWeek) && v == 7 {
                0
            } else {
                v
            };
            bits |= 1u64 << normalized;
            v += step;
        }
    }
    Ok(bits)
}

impl CronSchedule {
    /// Parses a 5-field cron expression.
    pub fn parse(s: &str) -> Result<Self, WorkflowError> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(err(s, format!("expected 5 fields, found {}", fields.len())));
        }
        Ok(CronSchedule {
            source: s.trim().to_string(),
            minutes: parse_field(Field::Minute, fields[0], s)?,
            hours: parse_field(Field::Hour, fields[1], s)? as u32,
            days_of_month: parse_field(Field::DayOfMonth, fields[2], s)? as u32,
            months: parse_field(Field::Month, fields[3], s)? as u16,
            days_of_week: parse_field(Field::DayOfWeek, fields[4], s)? as u8,
            dom_star: fields[2].starts_with('*'),
            dow_star: fields[4].starts_with('*'),
        })
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        if self.months & (1 << date.month()) == 0 {
            return false;
        }
        let dom = self.days_of_month & (1 << date.day()) != 0;
        let dow = self.days_of_week & (1 << date.weekday().num_days_from_sunday()) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// Whether the schedule fires at `t` (seconds are ignored).
    pub fn matches(&self, t: DateTime<Utc>) -> bool {
        self.day_matches(t.date_naive())
            && self.hours & (1 << t.hour()) != 0
            && self.minutes & (1 << t.minute()) != 0
    }

    /// The first firing time strictly after `t` (minute resolution), searched
    /// for about five years ahead. Iterates day by day, then picks the first
    /// matching hour and minute within the day.
    pub fn next_after(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let start = t
            .with_second(0)
            .and_then(|x| x.with_nanosecond(0))
            .unwrap_or(t)
            + Duration::minutes(1);
        let first_day = start.date_naive();
        let mut day = first_day;
        for _ in 0..SEARCH_DAYS {
            if self.day_matches(day) {
                let (min_hour, min_minute) = if day == first_day {
                    (start.hour(), start.minute())
                } else {
                    (0, 0)
                };
                for hour in min_hour..24 {
                    if self.hours & (1 << hour) == 0 {
                        continue;
                    }
                    let from = if hour == min_hour { min_minute } else { 0 };
                    if let Some(minute) = (from..60).find(|m| self.minutes & (1u64 << m) != 0) {
                        let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
                        return Some(Utc.from_utc_datetime(&day.and_time(time)));
                    }
                }
            }
            day = day.succ_opt()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn next(expr: &str, from: &str) -> String {
        CronSchedule::parse(expr)
            .unwrap()
            .next_after(at(from))
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn every_15_minutes() {
        assert_eq!(
            next("*/15 * * * *", "2024-01-01T10:07:00Z"),
            "2024-01-01T10:15:00+00:00"
        );
        assert_eq!(
            next("*/15 * * * *", "2024-01-01T10:15:00Z"),
            "2024-01-01T10:30:00+00:00",
            "strictly after"
        );
        assert_eq!(
            next("*/15 * * * *", "2024-01-01T23:59:30Z"),
            "2024-01-02T00:00:00+00:00"
        );
    }

    #[test]
    fn weekly_monday_midnight() {
        // 2024-01-03 is a Wednesday
        assert_eq!(
            next("0 0 * * 1", "2024-01-03T12:00:00Z"),
            "2024-01-08T00:00:00+00:00"
        );
        assert_eq!(
            next("0 0 * * MON", "2024-01-08T00:00:00Z"),
            "2024-01-15T00:00:00+00:00"
        );
    }

    #[test]
    fn dom_or_dow_semantics() {
        // fires on the 1st of the month OR on Mondays
        let c = CronSchedule::parse("0 12 1 * MON").unwrap();
        // 2024-01-01 is a Monday and the 1st
        assert!(c.matches(at("2024-01-01T12:00:00Z")));
        // 2024-01-08 is a Monday, not the 1st
        assert!(c.matches(at("2024-01-08T12:00:00Z")));
        // 2024-02-01 is a Thursday but the 1st
        assert!(c.matches(at("2024-02-01T12:00:00Z")));
        assert!(!c.matches(at("2024-01-09T12:00:00Z")));
        assert_eq!(
            next("0 12 1 * MON", "2024-01-29T13:00:00Z"),
            "2024-02-01T12:00:00+00:00"
        );
        assert_eq!(
            next("0 12 1 * MON", "2024-02-01T12:00:00Z"),
            "2024-02-05T12:00:00+00:00"
        );
    }

    #[test]
    fn dom_star_step_means_and() {
        // `*/2` in dom counts as unrestricted-star (vixie cron): AND with dow
        let c = CronSchedule::parse("0 0 */2 * 1").unwrap();
        // 2024-01-01 Monday, day 1 (odd -> in */2 from 1)
        assert!(c.matches(at("2024-01-01T00:00:00Z")));
        // 2024-01-08 Monday, day 8 (even -> not in */2)
        assert!(!c.matches(at("2024-01-08T00:00:00Z")));
        // 2024-01-03 Wednesday, day 3
        assert!(!c.matches(at("2024-01-03T00:00:00Z")));
    }

    #[test]
    fn leap_day() {
        assert_eq!(
            next("0 0 29 2 *", "2024-03-01T00:00:00Z"),
            "2028-02-29T00:00:00+00:00"
        );
        assert_eq!(
            next("0 0 29 2 *", "2023-06-01T00:00:00Z"),
            "2024-02-29T00:00:00+00:00"
        );
    }

    #[test]
    fn impossible_date_returns_none() {
        let c = CronSchedule::parse("0 0 31 2 *").unwrap();
        assert_eq!(c.next_after(at("2024-01-01T00:00:00Z")), None);
    }

    #[test]
    fn ranges_lists_and_steps() {
        let c = CronSchedule::parse("1-30/5 8,17 * * *").unwrap();
        assert!(c.matches(at("2024-05-05T08:01:00Z")));
        assert!(c.matches(at("2024-05-05T17:26:00Z")));
        assert!(!c.matches(at("2024-05-05T17:31:00Z")));
        assert!(!c.matches(at("2024-05-05T09:01:00Z")));
        let c = CronSchedule::parse("5/10 * * * *").unwrap();
        for m in [5, 15, 25, 35, 45, 55] {
            assert!(c.matches(at(&format!("2024-05-05T00:{m:02}:00Z"))));
        }
        assert!(!c.matches(at("2024-05-05T00:00:00Z")));
    }

    #[test]
    fn names_case_insensitive_and_sunday_seven() {
        let c = CronSchedule::parse("30 5 * jan,Dec sun").unwrap();
        // 2023-12-31 is a Sunday
        assert!(c.matches(at("2023-12-31T05:30:00Z")));
        assert!(!c.matches(at("2023-11-26T05:30:00Z")));
        let c7 = CronSchedule::parse("0 0 * * 7").unwrap();
        assert!(c7.matches(at("2023-12-31T00:00:00Z")));
        let range = CronSchedule::parse("0 0 * * 5-7").unwrap();
        assert!(range.matches(at("2023-12-29T00:00:00Z"))); // Fri
        assert!(range.matches(at("2023-12-30T00:00:00Z"))); // Sat
        assert!(range.matches(at("2023-12-31T00:00:00Z"))); // Sun
        assert!(!range.matches(at("2024-01-01T00:00:00Z"))); // Mon
        let names = CronSchedule::parse("0 0 * MAR-MAY MON-FRI").unwrap();
        assert!(names.matches(at("2024-04-01T00:00:00Z")));
        assert!(!names.matches(at("2024-04-06T00:00:00Z")));
    }

    #[test]
    fn next_after_skips_to_next_hour_and_month() {
        assert_eq!(
            next("30 9 * * *", "2024-01-01T09:30:00Z"),
            "2024-01-02T09:30:00+00:00"
        );
        assert_eq!(
            next("0 6 15 7 *", "2024-07-15T06:00:00Z"),
            "2025-07-15T06:00:00+00:00"
        );
        assert_eq!(
            next("0 * * * *", "2024-12-31T23:00:00Z"),
            "2025-01-01T00:00:00+00:00"
        );
        assert_eq!(
            next("* * * * *", "2024-01-01T00:00:59Z"),
            "2024-01-01T00:01:00+00:00"
        );
    }

    #[test]
    fn invalid_expressions() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "a * * * *",
            "1,,2 * * * *",
            "@daily",
            "* * * FOO *",
        ] {
            assert!(CronSchedule::parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn roundtrips_through_json() {
        let c = CronSchedule::parse("*/5 1-3 * * MON").unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: CronSchedule = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
