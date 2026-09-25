//! Bank-transfer QR codes (spec §10.4, A25): **SPAYD** 1.0 for CZK (Czech banking
//! association's "Short Payment Descriptor") and **PAY by square** 1.2.0 for EUR (Slovak
//! Banking Association), rendered as SVG.
//!
//! PAY by square, per the official specification: the fields of one payment order joined by
//! tabs in the fixed order, prefixed with their CRC32 (little endian), compressed with raw
//! LZMA1 (lc=3, lp=0, pb=2, dictionary 2^17, no stream header), preceded by the 2-byte
//! header (type 0, version 1.2.0 = 2, document 0, reserved 0) and the 2-byte little-endian
//! length of the checksummed payload, encoded as base32hex without padding. The output is
//! byte-identical to the reference `bysquare` library (golden vectors in `fixtures/qr/`): the
//! LZMA stream comes from a port of the LZMA SDK encoder that library uses ([`super::lzma`]);
//! other encoders (liblzma) produce different, equally decodable bytes.

use platform::Error;
use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a bank-transfer QR code carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer<'a> {
    pub iban: &'a str,
    pub bic: Option<&'a str>,
    /// Two decimal places (CZK, EUR).
    pub amount_minor: i64,
    /// ISO 4217, upper case.
    pub currency: &'a str,
    /// Digits only, at most 10 (A25).
    pub variable_symbol: &'a str,
    /// Message for the recipient.
    pub message: &'a str,
    /// The account holder (PAY by square 1.2.0 requires it).
    pub beneficiary: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QrKind {
    /// Czech QR payment (CZK).
    Spayd,
    /// Slovak QR payment (EUR).
    PayBySquare,
}

impl QrKind {
    /// The QR standard banking apps of that currency's market read.
    pub fn for_currency(currency: &str) -> Option<Self> {
        match currency {
            "CZK" => Some(Self::Spayd),
            "EUR" => Some(Self::PayBySquare),
            _ => None,
        }
    }
}

/// The QR payload for `t` in the standard of its currency (`None` for other currencies).
pub fn payload(t: &Transfer<'_>) -> Result<Option<(QrKind, String)>, Error> {
    Ok(match QrKind::for_currency(t.currency) {
        Some(QrKind::Spayd) => Some((QrKind::Spayd, spayd(t))),
        Some(QrKind::PayBySquare) => Some((QrKind::PayBySquare, pay_by_square(t)?)),
        None => None,
    })
}

// ---------------------------------------------------------------------------------------
// SPAYD

/// SPAYD value escaping: `*` separates fields, so it is written as `%2A` (A25); control
/// characters become spaces.
fn spayd_escape(v: &str, max_chars: usize) -> String {
    v.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect::<String>()
        .replace('*', "%2A")
}

/// `SPD*1.0*ACC:<IBAN>[+<BIC>]*AM:<amount>*CC:<currency>*X-VS:<vs>*MSG:<message>*RN:<name>`.
pub fn spayd(t: &Transfer<'_>) -> String {
    let mut acc = t.iban.to_owned();
    if let Some(bic) = t.bic {
        acc.push('+');
        acc.push_str(bic);
    }
    let mut out = format!(
        "SPD*1.0*ACC:{}*AM:{}.{:02}*CC:{}",
        spayd_escape(&acc, 46),
        t.amount_minor / 100,
        t.amount_minor % 100,
        spayd_escape(t.currency, 3),
    );
    if !t.variable_symbol.is_empty() {
        out.push_str("*X-VS:");
        out.push_str(&spayd_escape(t.variable_symbol, 10));
    }
    if !t.message.is_empty() {
        out.push_str("*MSG:");
        out.push_str(&spayd_escape(t.message, 60));
    }
    if !t.beneficiary.is_empty() {
        out.push_str("*RN:");
        out.push_str(&spayd_escape(t.beneficiary, 35));
    }
    out
}

// ---------------------------------------------------------------------------------------
// PAY by square 1.2.0

/// Fields may not contain the separator.
fn field(v: &str) -> String {
    v.replace('\t', " ")
}

/// The amount as the reference encoder writes it: no trailing zeros, `.` separator.
fn decimal(minor: i64) -> String {
    let (whole, cents) = (minor / 100, minor % 100);
    match cents {
        0 => whole.to_string(),
        c if c % 10 == 0 => format!("{whole}.{}", c / 10),
        c => format!("{whole}.{c:02}"),
    }
}

/// The tab-separated data model of one payment order (spec §3.3; 1.2.0 adds the beneficiary
/// block after the payments).
pub fn pay_by_square_fields(t: &Transfer<'_>) -> String {
    [
        String::new(), // invoice id
        "1".into(),    // payments count
        "1".into(),    // payment options: payment order
        decimal(t.amount_minor),
        field(t.currency),
        String::new(), // due date
        field(t.variable_symbol),
        String::new(), // constant symbol
        String::new(), // specific symbol
        String::new(), // originator's reference
        field(&deburr(t.message)),
        "1".into(), // bank accounts count
        field(t.iban),
        field(t.bic.unwrap_or_default()),
        "0".into(), // no standing order extension
        "0".into(), // no direct debit extension
        field(&deburr(t.beneficiary)),
        String::new(), // beneficiary street
        String::new(), // beneficiary city
    ]
    .join("\t")
}

pub fn pay_by_square(t: &Transfer<'_>) -> Result<String, Error> {
    let fields = pay_by_square_fields(t);
    let mut checked = crc32fast::hash(fields.as_bytes()).to_le_bytes().to_vec();
    checked.extend_from_slice(fields.as_bytes());
    let len = u16::try_from(checked.len())
        .ok()
        .filter(|l| *l < u16::MAX)
        .ok_or_else(|| Error::Internal("PAY by square payload too large".into()))?;
    // Header: by square type 0, version 1.2.0 (2), document type 0, reserved 0.
    let mut data = vec![0x02, 0x00];
    data.extend_from_slice(&len.to_le_bytes());
    data.extend(super::lzma::compress_raw(&checked));
    Ok(base32hex(&data))
}

fn base32hex(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";
    let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for b in data {
        buffer = (buffer << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Latin-1 Supplement and Latin Extended-A letters to basic Latin, combining marks dropped
/// (what the reference encoder does to the note and the beneficiary).
pub fn deburr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match deburr_char(c) {
            Some(r) => out.push_str(r),
            None if matches!(u32::from(c), 0x300..=0x36f | 0xfe20..=0xfe23 | 0x20d0..=0x20f0) => {}
            None => out.push(c),
        }
    }
    out
}

fn deburr_char(c: char) -> Option<&'static str> {
    Some(match c {
        'À'..='Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => "C",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'Ð' | 'Ď' | 'Đ' => "D",
        'ð' | 'ď' | 'đ' => "d",
        'È'..='Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' => "E",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => "G",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'Ĥ' | 'Ħ' => "H",
        'ĥ' | 'ħ' => "h",
        'Ì'..='Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' => "I",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'Ĵ' => "J",
        'ĵ' => "j",
        'Ķ' => "K",
        'ķ' | 'ĸ' => "k",
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' => "L",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' | 'Ŋ' => "N",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŋ' => "n",
        'Ò'..='Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' => "O",
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'Ŕ' | 'Ŗ' | 'Ř' => "R",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'Ś' | 'Ŝ' | 'Ş' | 'Š' => "S",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'Ţ' | 'Ť' | 'Ŧ' => "T",
        'ţ' | 'ť' | 'ŧ' => "t",
        'Ù'..='Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' => "U",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'Ŵ' => "W",
        'ŵ' => "w",
        'Ý' | 'Ŷ' | 'Ÿ' => "Y",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ź' | 'ż' | 'ž' => "z",
        'Æ' => "Ae",
        'æ' => "ae",
        'Þ' => "Th",
        'þ' => "th",
        'ß' | 'ſ' => "ss",
        'Ĳ' => "IJ",
        'ĳ' => "ij",
        'Œ' => "Oe",
        'œ' => "oe",
        'ŉ' => "'n",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------
// Rendering

/// The QR code as an inline SVG element (no XML declaration), labelled for screen readers.
pub fn svg(payload: &str, label: &str) -> Result<String, Error> {
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::M)
        .map_err(|e| Error::Internal(format!("qr: {e}")))?;
    let image = code
        .render::<svg::Color<'_>>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build();
    let label: String = label
        .chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            c => c.to_string(),
        })
        .collect();
    let start = image
        .find("<svg")
        .ok_or_else(|| Error::Internal("qr: no svg element".into()))?;
    Ok(image[start..].replacen(
        "<svg ",
        &format!("<svg role=\"img\" aria-label=\"{label}\" "),
        1,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn transfer(c: &Value) -> Transfer<'_> {
        Transfer {
            iban: c["iban"].as_str().unwrap(),
            bic: c["bic"].as_str(),
            amount_minor: c["amount_minor"].as_i64().unwrap(),
            currency: "EUR",
            variable_symbol: c["variable_symbol"].as_str().unwrap(),
            message: c["note"].as_str().unwrap(),
            beneficiary: c["beneficiary_name"].as_str().unwrap(),
        }
    }

    /// Golden vectors from the reference `bysquare` library (A25): byte for byte.
    #[test]
    fn pay_by_square_matches_the_reference_vectors() {
        let vectors: Value =
            serde_json::from_str(include_str!("../../../../fixtures/qr/paybysquare.json")).unwrap();
        assert_eq!(vectors["version"], "1.2.0");
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() >= 5);
        for c in cases {
            let t = transfer(c);
            let name = c["name"].as_str().unwrap();
            assert_eq!(
                pay_by_square_fields(&t),
                c["payload"].as_str().unwrap(),
                "{name}: fields"
            );
            assert_eq!(
                pay_by_square(&t).unwrap(),
                c["qr"].as_str().unwrap(),
                "{name}"
            );
        }
    }

    #[test]
    fn decimals_like_the_reference() {
        assert_eq!(decimal(1290), "12.9");
        assert_eq!(decimal(10000), "100");
        assert_eq!(decimal(5), "0.05");
        assert_eq!(decimal(12345678), "123456.78");
    }

    #[test]
    fn base32hex_without_padding() {
        // RFC 4648 §10 test vectors (base32hex), padding removed.
        for (input, want) in [
            ("", ""),
            ("f", "CO"),
            ("fo", "CPNG"),
            ("foo", "CPNMU"),
            ("foob", "CPNMUOG"),
            ("fooba", "CPNMUOJ1"),
            ("foobar", "CPNMUOJ1E8"),
        ] {
            assert_eq!(base32hex(input.as_bytes()), want, "{input}");
        }
    }

    #[test]
    fn spayd_format_and_escaping() {
        let t = Transfer {
            iban: "CZ6508000000192000145399",
            bic: Some("GIBACZPX"),
            amount_minor: 48_050,
            currency: "CZK",
            variable_symbol: "100001",
            message: "Obchod *Demo*\nobjednávka",
            beneficiary: "Demo s.r.o.",
        };
        assert_eq!(
            spayd(&t),
            "SPD*1.0*ACC:CZ6508000000192000145399+GIBACZPX*AM:480.50*CC:CZK*X-VS:100001\
             *MSG:Obchod %2ADemo%2A objednávka*RN:Demo s.r.o."
        );
        let plain = Transfer {
            bic: None,
            amount_minor: 100,
            message: "",
            beneficiary: "",
            ..t
        };
        assert_eq!(
            spayd(&plain),
            "SPD*1.0*ACC:CZ6508000000192000145399*AM:1.00*CC:CZK*X-VS:100001"
        );
        // Every field value is escaped, so a crafted message cannot inject a field.
        let evil = Transfer {
            message: "x*AM:1.00",
            ..t
        };
        assert_eq!(spayd(&evil).matches("*AM:").count(), 1);
    }

    #[test]
    fn svg_is_an_inline_labelled_element() {
        let s = svg(
            "SPD*1.0*ACC:CZ6508000000192000145399*AM:1.00",
            "QR \"platba\"",
        )
        .unwrap();
        assert!(s.starts_with("<svg role=\"img\" aria-label=\"QR &quot;platba&quot;\" "));
        assert!(!s.contains("<?xml"));
        assert!(s.trim_end().ends_with("</svg>"));
        assert!(!s.contains("<script"));
    }

    #[test]
    fn kind_by_currency() {
        assert_eq!(QrKind::for_currency("CZK"), Some(QrKind::Spayd));
        assert_eq!(QrKind::for_currency("EUR"), Some(QrKind::PayBySquare));
        assert_eq!(QrKind::for_currency("PLN"), None);
    }
}
