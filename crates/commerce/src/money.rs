//! Money (spec §10.1, D25): integer minor units plus an ISO 4217 currency. No floats anywhere.
//!
//! Arithmetic mirrors `std`'s checked operations: `None` on overflow or a currency mismatch.
//! Products of amounts and quantities or rates are computed in `i128` and narrowed back.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Currencies used by EU markets: the euro and the member states' own currencies.
/// `BGN` is historic (Bulgaria adopted the euro on 2026-01-01) and kept for old documents.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Bgn,
    Czk,
    Dkk,
    Eur,
    Huf,
    Pln,
    Ron,
    Sek,
}

impl Currency {
    pub const ALL: [Self; 8] = [
        Self::Bgn,
        Self::Czk,
        Self::Dkk,
        Self::Eur,
        Self::Huf,
        Self::Pln,
        Self::Ron,
        Self::Sek,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Bgn => "BGN",
            Self::Czk => "CZK",
            Self::Dkk => "DKK",
            Self::Eur => "EUR",
            Self::Huf => "HUF",
            Self::Pln => "PLN",
            Self::Ron => "RON",
            Self::Sek => "SEK",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.code() == code)
    }

    /// Digits after the decimal point (ISO 4217). All supported currencies use 2; HUF too,
    /// even though its coins have no fractional unit.
    pub fn exponent(self) -> u32 {
        2
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// Formatting locales supported by the storefront and documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    Cs,
    Sk,
    En,
}

impl Locale {
    /// `cs`, `cs-CZ` -> Cs; `sk`, `sk-SK` -> Sk; anything else -> En.
    pub fn from_tag(tag: &str) -> Self {
        match tag.split('-').next() {
            Some("cs") => Self::Cs,
            Some("sk") => Self::Sk,
            _ => Self::En,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub struct Money {
    #[serde(rename = "amount_minor")]
    pub minor: i64,
    pub currency: Currency,
}

/// The API shape of an amount (spec §8.1): `{amount_minor, currency, formatted}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct MoneyView {
    pub amount_minor: i64,
    pub currency: Currency,
    #[schema(example = "1 290,00 Kč")]
    pub formatted: String,
}

impl Money {
    pub fn new(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }

    pub fn zero(currency: Currency) -> Self {
        Self::new(0, currency)
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        (self.currency == other.currency)
            .then(|| self.minor.checked_add(other.minor))
            .flatten()
            .map(|minor| Self::new(minor, self.currency))
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        (self.currency == other.currency)
            .then(|| self.minor.checked_sub(other.minor))
            .flatten()
            .map(|minor| Self::new(minor, self.currency))
    }

    pub fn checked_mul(self, factor: i64) -> Option<Self> {
        self.minor
            .checked_mul(factor)
            .map(|minor| Self::new(minor, self.currency))
    }

    pub fn view(self, locale: Locale) -> MoneyView {
        MoneyView {
            amount_minor: self.minor,
            currency: self.currency,
            formatted: self.format(locale),
        }
    }

    /// Locale formatting matching `Intl.NumberFormat` (CLDR): `1 290,00 Kč` and `12,90 €`
    /// for cs/sk, `€12.90` and `CZK 1,290.00` for en. The spaces are U+00A0 (no-break), so a
    /// price never wraps between digits or before its symbol.
    pub fn format(self, locale: Locale) -> String {
        const NBSP: char = '\u{a0}';
        let exp = self.currency.exponent();
        let scale = 10u64.pow(exp);
        let abs = self.minor.unsigned_abs();
        let (int, frac) = (abs / scale, abs % scale);
        let (group, decimal) = match locale {
            Locale::Cs | Locale::Sk => (NBSP, ','),
            Locale::En => (',', '.'),
        };
        let digits = int.to_string();
        let mut number = String::with_capacity(digits.len() + 8);
        for (i, d) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                number.push(group);
            }
            number.push(d);
        }
        if exp > 0 {
            let _ = write!(number, "{decimal}{frac:0width$}", width = exp as usize);
        }
        let sign = if self.minor < 0 { "-" } else { "" };
        match (locale, self.currency) {
            (Locale::Cs, Currency::Czk) => format!("{sign}{number}{NBSP}Kč"),
            (Locale::Cs | Locale::Sk, Currency::Eur) => format!("{sign}{number}{NBSP}€"),
            (Locale::Cs | Locale::Sk, c) => format!("{sign}{number}{NBSP}{c}"),
            (Locale::En, Currency::Eur) => format!("{sign}€{number}"),
            (Locale::En, c) => format!("{sign}{c}{NBSP}{number}"),
        }
    }
}

/// `n / d` rounded half away from zero (`d > 0`). The rounding the spec calls
/// `round_half_up` for VAT; symmetric, so a negative amount rounds like its positive twin.
pub fn div_round_half_up(n: i128, d: i128) -> i128 {
    debug_assert!(d > 0);
    let q = n / d;
    let r = n % d;
    if 2 * r.abs() >= d { q + n.signum() } else { q }
}

/// Narrows an `i128` intermediate back to minor units.
pub fn to_minor(v: i128) -> Option<i64> {
    i64::try_from(v).ok()
}

/// Splits `total` (>= 0) proportionally to `weights` (>= 0, sum > 0) with the largest-remainder
/// method: every share is the floor of its exact share, and the leftover units go one each to
/// the largest fractional remainders (ties: lower index first). The shares sum to `total`,
/// each is within 1 of its exact share and never exceeds its weight when `total <= Σweights`.
/// Deterministic for the same input. `None` for negative input or all-zero weights.
pub fn allocate(total: i64, weights: &[i64]) -> Option<Vec<i64>> {
    if total < 0 || weights.iter().any(|w| *w < 0) {
        return None;
    }
    let sum: i128 = weights.iter().map(|w| i128::from(*w)).sum();
    if sum == 0 {
        return None;
    }
    let total = i128::from(total);
    let mut shares = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    for (i, w) in weights.iter().enumerate() {
        let exact = total * i128::from(*w);
        shares.push(exact / sum);
        remainders.push((exact % sum, i));
    }
    let mut leftover = total - shares.iter().sum::<i128>();
    // Largest remainder first; equal remainders keep index order.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in remainders {
        if leftover == 0 {
            break;
        }
        shares[i] += 1;
        leftover -= 1;
    }
    shares.into_iter().map(to_minor).collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NBSP: &str = "\u{a0}";

    fn nb(s: &str) -> String {
        s.replace(' ', NBSP)
    }

    #[test]
    fn formats_per_locale() {
        let czk = Money::new(129_000, Currency::Czk);
        assert_eq!(czk.format(Locale::Cs), nb("1 290,00 Kč"));
        assert_eq!(czk.format(Locale::Sk), nb("1 290,00 CZK"));
        assert_eq!(czk.format(Locale::En), nb("CZK 1,290.00"));
        let eur = Money::new(1290, Currency::Eur);
        assert_eq!(eur.format(Locale::Sk), nb("12,90 €"));
        assert_eq!(eur.format(Locale::Cs), nb("12,90 €"));
        assert_eq!(eur.format(Locale::En), "€12.90");
        assert_eq!(
            Money::new(-123_456_789, Currency::Eur).format(Locale::En),
            "-€1,234,567.89"
        );
        assert_eq!(
            Money::new(5, Currency::Pln).format(Locale::Cs),
            nb("0,05 PLN")
        );
        assert_eq!(
            Money::new(100_000_000, Currency::Huf).format(Locale::Sk),
            nb("1 000 000,00 HUF")
        );
        assert_eq!(
            Money::new(0, Currency::Czk).format(Locale::Cs),
            nb("0,00 Kč")
        );
        assert_eq!(Locale::from_tag("cs-CZ"), Locale::Cs);
        assert_eq!(Locale::from_tag("sk"), Locale::Sk);
        assert_eq!(Locale::from_tag("de"), Locale::En);
    }

    #[test]
    fn view_and_serde() {
        let m = Money::new(12_900, Currency::Czk);
        let v = serde_json::to_value(m.view(Locale::Cs)).unwrap();
        assert_eq!(v["amount_minor"], 12_900);
        assert_eq!(v["currency"], "CZK");
        assert_eq!(v["formatted"], nb("129,00 Kč"));
        let back: Money =
            serde_json::from_value(serde_json::json!({"amount_minor": 5, "currency": "EUR"}))
                .unwrap();
        assert_eq!(back, Money::new(5, Currency::Eur));
        assert!(serde_json::from_value::<Currency>(serde_json::json!("USD")).is_err());
        assert_eq!(Currency::parse("RON"), Some(Currency::Ron));
        assert_eq!(Currency::parse("ron"), None);
    }

    #[test]
    fn checked_arithmetic() {
        let a = Money::new(100, Currency::Czk);
        assert_eq!(a.checked_add(a), Some(Money::new(200, Currency::Czk)));
        assert_eq!(a.checked_sub(a), Some(Money::zero(Currency::Czk)));
        assert_eq!(a.checked_add(Money::new(1, Currency::Eur)), None);
        assert_eq!(
            Money::new(i64::MAX, Currency::Eur).checked_add(Money::new(1, Currency::Eur)),
            None
        );
        assert_eq!(Money::new(i64::MAX, Currency::Eur).checked_mul(2), None);
        assert_eq!(a.checked_mul(3), Some(Money::new(300, Currency::Czk)));
    }

    #[test]
    fn rounding_half_away_from_zero() {
        assert_eq!(div_round_half_up(5, 2), 3);
        assert_eq!(div_round_half_up(4, 2), 2);
        assert_eq!(div_round_half_up(7, 3), 2);
        assert_eq!(div_round_half_up(-5, 2), -3);
        assert_eq!(div_round_half_up(-7, 3), -2);
        // 121 Kč incl. 21 % VAT: 12 100 * 2100 / 12 100 = 2100 minor.
        assert_eq!(div_round_half_up(12_100 * 2100, 12_100), 2100);
    }

    #[test]
    fn allocation_examples() {
        assert_eq!(allocate(100, &[1, 1, 1]), Some(vec![34, 33, 33]));
        assert_eq!(allocate(10, &[500, 300, 200]), Some(vec![5, 3, 2]));
        assert_eq!(allocate(0, &[5, 5]), Some(vec![0, 0]));
        assert_eq!(allocate(2, &[1, 1, 1]), Some(vec![1, 1, 0]));
        assert_eq!(allocate(5, &[0, 0]), None);
        assert_eq!(allocate(-1, &[1]), None);
        assert_eq!(allocate(1, &[1, -1, 3]), None);
    }

    proptest! {
        #[test]
        fn allocation_sums_is_fair_and_deterministic(
            total in 0i64..10_000_000_000,
            weights in prop::collection::vec(0i64..10_000_000_000, 1..40),
        ) {
            prop_assume!(weights.iter().any(|w| *w > 0));
            let shares = allocate(total, &weights).unwrap();
            prop_assert_eq!(shares.iter().sum::<i64>(), total);
            let sum: i128 = weights.iter().map(|w| i128::from(*w)).sum();
            for (s, w) in shares.iter().zip(&weights) {
                prop_assert!(*s >= 0);
                let exact = i128::from(total) * i128::from(*w);
                // |share * sum - exact| < sum, i.e. within one unit of the exact share.
                prop_assert!((i128::from(*s) * sum - exact).abs() < sum);
                if i128::from(total) <= sum {
                    prop_assert!(s <= w);
                }
            }
            prop_assert_eq!(allocate(total, &weights).unwrap(), shares);
        }

        #[test]
        fn format_roundtrips_digits(minor in -1_000_000_000_000i64..1_000_000_000_000) {
            for locale in [Locale::Cs, Locale::Sk, Locale::En] {
                let s = Money::new(minor, Currency::Czk).format(locale);
                let digits: String = s.chars().filter(char::is_ascii_digit).collect();
                prop_assert_eq!(digits.parse::<i64>().unwrap(), minor.abs());
                prop_assert_eq!(s.starts_with('-'), minor < 0);
            }
        }
    }
}
