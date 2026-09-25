//! Czech/Slovak civil time (CET/CEST) for document dates: the issue date and DUZP are
//! calendar dates where the merchant is, not UTC dates. EU rule: summer time from the last
//! Sunday of March to the last Sunday of October, switching at 01:00 UTC.
//! ponytail: CZ/SK only; take the tenant's zone (chrono-tz) when markets outside CET arrive.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};

fn last_sunday(year: i32, month: u32) -> NaiveDate {
    let first_next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .unwrap_or_default();
    let last = first_next - Duration::days(1);
    last - Duration::days(i64::from(last.weekday().num_days_from_sunday()))
}

/// The UTC offset in hours at `t`.
fn offset(t: DateTime<Utc>) -> i64 {
    let y = t.year();
    let switch = |d: NaiveDate| d.and_hms_opt(1, 0, 0).unwrap_or_default().and_utc();
    let (start, end) = (switch(last_sunday(y, 3)), switch(last_sunday(y, 10)));
    if t >= start && t < end { 2 } else { 1 }
}

pub fn local(t: DateTime<Utc>) -> chrono::NaiveDateTime {
    t.naive_utc() + Duration::hours(offset(t))
}

/// The instant of a local Czech/Slovak wall-clock time. In the autumn hour that occurs twice
/// the summer reading wins; a spring-gap time maps one hour later.
pub fn from_local(t: chrono::NaiveDateTime) -> DateTime<Utc> {
    let summer = t.and_utc() - Duration::hours(2);
    if offset(summer) == 2 {
        summer
    } else {
        t.and_utc() - Duration::hours(1)
    }
}

pub fn date(t: DateTime<Utc>) -> NaiveDate {
    local(t).date()
}

pub fn time(t: DateTime<Utc>) -> NaiveTime {
    local(t).time()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap_or_default()
    }

    #[test]
    fn follows_cet_and_cest() {
        assert_eq!(
            last_sunday(2026, 3),
            NaiveDate::from_ymd_opt(2026, 3, 29).unwrap_or_default()
        );
        assert_eq!(last_sunday(2026, 10).weekday(), Weekday::Sun);
        // Winter: UTC+1; 23:30 UTC on 31 Dec is already the next day.
        assert_eq!(date(at("2026-12-31T23:30:00Z")).to_string(), "2027-01-01");
        // Summer: UTC+2.
        assert_eq!(date(at("2026-09-25T22:30:00Z")).to_string(), "2026-09-26");
        assert_eq!(date(at("2026-09-25T21:30:00Z")).to_string(), "2026-09-25");
        assert_eq!(time(at("2026-09-25T12:45:00Z")).to_string(), "14:45:00");
        // The switch itself (01:00 UTC).
        assert_eq!(offset(at("2026-03-29T00:59:00Z")), 1);
        assert_eq!(offset(at("2026-03-29T01:00:00Z")), 2);
        assert_eq!(offset(at("2026-10-25T01:00:00Z")), 1);
    }
}
