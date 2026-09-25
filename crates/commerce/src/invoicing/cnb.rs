//! ČNB daily FX fixing (spec §7.4, A17): the statutory rate for the CZK recap of non-CZK
//! documents is the latest fixing published on or before the taxable supply date.
//!
//! Source: `denni_kurz.txt?date=DD.MM.YYYY` (public, plain text). Asked for a date, ČNB
//! answers with the fixing valid on that date: on weekends and holidays the previous business
//! day's, whose own date is in the header. A fixing is published around 14:30 on business
//! days, so for a taxable supply today an earlier fixing is only accepted once today's would
//! have been published ([`acceptable`]); until then issuing is retried.
//!
//! ```text
//! 25.09.2026 #186
//! země|měna|množství|kód|kurz
//! EMU|euro|1|EUR|24,305
//! ```

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use platform::Error;
use sqlx::PgPool;

use super::document::Rate;
use super::prague;
use crate::money::Currency;

/// After this (Prague time) today's fixing is published; an older one is then final for
/// today only if today is a holiday.
const PUBLISHED_BY: (u32, u32) = (14, 45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixing {
    pub date: NaiveDate,
    /// (ISO code, amount, CZK per amount × 1000)
    pub rates: Vec<(String, i32, i64)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unexpected ČNB fixing format: {0}")]
pub struct ParseError(pub String);

/// "24,305" → 24305 (always three decimals at ČNB; fewer are padded).
fn milli(s: &str) -> Option<i64> {
    let (int, frac) = s.trim().split_once(',').unwrap_or((s.trim(), ""));
    if frac.len() > 3 || int.is_empty() {
        return None;
    }
    let int: i64 = int.parse().ok()?;
    let frac: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<3}").parse().ok()?
    };
    Some(int * 1000 + frac)
}

pub fn parse(text: &str) -> Result<Fixing, ParseError> {
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(|| ParseError("empty".into()))?;
    let date_s = header
        .split_whitespace()
        .next()
        .ok_or_else(|| ParseError("no date".into()))?;
    let date = NaiveDate::parse_from_str(date_s, "%d.%m.%Y")
        .map_err(|_| ParseError(format!("date {date_s}")))?;
    lines.next(); // column names
    let mut rates = Vec::new();
    for l in lines.filter(|l| !l.trim().is_empty()) {
        let cols: Vec<&str> = l.split('|').collect();
        let [_, _, amount, code, rate] = cols[..] else {
            return Err(ParseError(format!("row {l}")));
        };
        let amount: i32 = amount
            .trim()
            .parse()
            .ok()
            .filter(|a| *a > 0)
            .ok_or_else(|| ParseError(format!("amount {amount}")))?;
        let rate = milli(rate)
            .filter(|r| *r > 0)
            .ok_or_else(|| ParseError(format!("rate {rate}")))?;
        let code = code.trim();
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(ParseError(format!("code {code}")));
        }
        rates.push((code.to_owned(), amount, rate));
    }
    if rates.is_empty() {
        return Err(ParseError("no rates".into()));
    }
    Ok(Fixing { date, rates })
}

/// Whether `fixing_date` is the rate for a taxable supply on `duzp`, asked at `now`: the same
/// date always; an earlier one when `duzp` is in the past, today is a known weekend (no fixing
/// will be published), or today's fixing would have been published by now (today is a holiday).
pub fn acceptable(fixing_date: NaiveDate, duzp: NaiveDate, now: DateTime<Utc>) -> bool {
    let today = prague::date(now);
    let published = NaiveTime::from_hms_opt(PUBLISHED_BY.0, PUBLISHED_BY.1, 0)
        .is_some_and(|t| prague::time(now) >= t);
    let weekend = matches!(duzp.weekday(), Weekday::Sat | Weekday::Sun);
    fixing_date == duzp
        || (fixing_date < duzp && (duzp < today || (duzp == today && (weekend || published))))
}

/// Downloads the fixing valid on `date`.
pub async fn fetch(http: &reqwest::Client, url: &str, date: NaiveDate) -> Result<Fixing, Error> {
    let res = http
        .get(
            reqwest::Url::parse_with_params(url, &[("date", date.format("%d.%m.%Y").to_string())])
                .map_err(|e| Error::Internal(format!("CNB_RATES_URL: {e}")))?,
        )
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| Error::Unavailable(format!("ČNB rates: {e}")))?;
    if !res.status().is_success() {
        return Err(Error::Unavailable(format!(
            "ČNB rates: HTTP {}",
            res.status()
        )));
    }
    let text = res
        .text()
        .await
        .map_err(|e| Error::Unavailable(format!("ČNB rates: {e}")))?;
    parse(&text).map_err(|e| Error::Unavailable(e.to_string()))
}

/// Stores a fixing (idempotent; a republished rate replaces the stored one).
pub async fn store(db: &PgPool, f: &Fixing) -> Result<(), Error> {
    for (code, amount, rate) in &f.rates {
        sqlx::query!(
            "INSERT INTO platform.exchange_rates (currency, fixing_date, amount, rate_milli)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (currency, fixing_date)
             DO UPDATE SET amount = EXCLUDED.amount, rate_milli = EXCLUDED.rate_milli,
                           fetched_at = now()",
            code,
            f.date,
            amount,
            rate
        )
        .execute(db)
        .await?;
    }
    Ok(())
}

/// The ČNB rate for a taxable supply of `currency` on `duzp`: a stored fixing of that very
/// date, else ČNB's answer for that date if [`acceptable`]. `Ok(None)`: not published yet
/// (the caller retries later and warns).
pub async fn rate_for(
    db: &PgPool,
    http: &reqwest::Client,
    url: &str,
    currency: Currency,
    duzp: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Option<Rate>, Error> {
    let stored = |date: NaiveDate| async move {
        sqlx::query!(
            "SELECT amount, rate_milli FROM platform.exchange_rates
             WHERE currency = $1 AND fixing_date = $2",
            currency.code(),
            date
        )
        .fetch_optional(db)
        .await
        .map(|r| {
            r.map(|r| Rate {
                currency,
                fixing_date: date,
                amount: r.amount,
                rate_milli: r.rate_milli,
            })
        })
    };
    if let Some(r) = stored(duzp).await? {
        return Ok(Some(r));
    }
    let fixing = fetch(http, url, duzp).await?;
    store(db, &fixing).await?;
    if !acceptable(fixing.date, duzp, now) {
        return Ok(None);
    }
    stored(fixing.date).await?.map_or_else(
        || {
            Err(Error::Unavailable(format!(
                "ČNB publishes no rate for {currency}"
            )))
        },
        |r| Ok(Some(r)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "25.09.2026 #186\nzemě|měna|množství|kód|kurz\n\
        Austrálie|dolar|1|AUD|15,012\nEMU|euro|1|EUR|24,305\nMaďarsko|forint|100|HUF|6,321\n";

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap_or_default()
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap_or_default()
    }

    #[test]
    fn parses_the_fixing() {
        let f = parse(SAMPLE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(f.date, d(2026, 9, 25));
        assert_eq!(
            f.rates,
            vec![
                ("AUD".into(), 1, 15_012),
                ("EUR".into(), 1, 24_305),
                ("HUF".into(), 100, 6_321)
            ]
        );
        assert!(parse("").is_err());
        assert!(parse("xx #1\na\n").is_err());
        assert!(parse("25.09.2026 #1\nh\nEMU|euro|1|EUR|abc\n").is_err());
        assert_eq!(milli("24,3"), Some(24_300));
    }

    #[test]
    fn earlier_fixings_are_accepted_only_when_final() {
        // A weekend has no same-day fixing: Friday is final from the first minute of Saturday.
        assert!(acceptable(
            d(2026, 9, 25),
            d(2026, 9, 26),
            at("2026-09-25T22:10:00Z")
        ));
        assert!(acceptable(
            d(2026, 9, 25),
            d(2026, 9, 27),
            at("2026-09-27T06:00:00Z")
        ));
        // A future weekend cannot use a rate before its supply date.
        assert!(!acceptable(
            d(2026, 9, 25),
            d(2026, 9, 27),
            at("2026-09-25T09:00:00Z")
        ));
        // Saturday 26 Sep: ČNB answers with Friday's fixing; the supply is in the past.
        assert!(acceptable(
            d(2026, 9, 25),
            d(2026, 9, 26),
            at("2026-09-28T08:00:00Z")
        ));
        // A supply today (a business day) before publication: wait for today's fixing.
        assert!(!acceptable(
            d(2026, 9, 24),
            d(2026, 9, 25),
            at("2026-09-25T09:00:00Z")
        ));
        // After 14:45 Prague (12:45 UTC in summer) the older fixing is final (a holiday).
        assert!(acceptable(
            d(2026, 9, 25),
            d(2026, 9, 28),
            at("2026-09-28T13:00:00Z")
        ));
        assert!(acceptable(
            d(2026, 9, 25),
            d(2026, 9, 25),
            at("2026-09-25T05:00:00Z")
        ));
        assert!(!acceptable(
            d(2026, 9, 26),
            d(2026, 9, 25),
            at("2026-09-28T05:00:00Z")
        ));
    }
}
