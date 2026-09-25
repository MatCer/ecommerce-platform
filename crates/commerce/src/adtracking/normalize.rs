//! Identifier normalization and hashing per vendor spec. Each platform matches hashed values
//! only if they were normalized exactly its way, so the rules differ on purpose:
//! - Meta CAPI: email trimmed + lowercased; phone digits only, with the country code, no `+`
//!   and no leading zeros (`(650)555-1212` → `16505551212`).
//! - Google (Data Manager API): email lowercased with all whitespace removed, and for
//!   `gmail.com`/`googlemail.com` the dots and any `+suffix` of the local part removed; phone
//!   E.164 with `+`.
//! - Seznam SEM: email trimmed + lowercased; phone E.164 with `+`.
//!
//! Hashes are SHA-256 as lowercase hex (Google is told `encoding: HEX`).

use sha2::{Digest, Sha256};

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Meta and Seznam: trimmed and lowercased; `None` if it does not look like an address.
pub fn email_basic(email: &str) -> Option<String> {
    let e = email.trim().to_lowercase();
    plausible_email(&e).then_some(e)
}

/// Google: all whitespace removed and lowercased; Gmail addresses lose the dots and the
/// `+suffix` of the local part.
pub fn email_google(email: &str) -> Option<String> {
    let e: String = email
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    if !plausible_email(&e) {
        return None;
    }
    let (local, domain) = e.rsplit_once('@')?;
    if matches!(domain, "gmail.com" | "googlemail.com") {
        let local = local.split('+').next().unwrap_or_default().replace('.', "");
        return (!local.is_empty()).then(|| format!("{local}@{domain}"));
    }
    Some(e)
}

fn plausible_email(e: &str) -> bool {
    e.len() <= 254
        && e.rsplit_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !e.contains(char::is_whitespace)
        })
}

/// Country calling codes of the markets the platform sells to (EU/EEA + CH, GB).
/// ponytail: a fixed table; use a phone-number library if markets outside Europe appear.
fn calling_code(country: &str) -> Option<&'static str> {
    Some(match country {
        "AT" => "43",
        "BE" => "32",
        "BG" => "359",
        "CH" => "41",
        "CY" => "357",
        "CZ" => "420",
        "DE" => "49",
        "DK" => "45",
        "EE" => "372",
        "ES" => "34",
        "FI" => "358",
        "FR" => "33",
        "GB" => "44",
        "GR" => "30",
        "HR" => "385",
        "HU" => "36",
        "IE" => "353",
        "IS" => "354",
        "IT" => "39",
        "LI" => "423",
        "LT" => "370",
        "LU" => "352",
        "LV" => "371",
        "MT" => "356",
        "NL" => "31",
        "NO" => "47",
        "PL" => "48",
        "PT" => "351",
        "RO" => "40",
        "SE" => "46",
        "SI" => "386",
        "SK" => "421",
        _ => return None,
    })
}

/// E.164 with `+` (Google, Seznam). A number without an international prefix (`+` or `00`)
/// is read as national in `country` (the shipping country): its trunk `0` is dropped and the
/// calling code added. `None` when it cannot be a valid E.164 number (8-15 digits).
pub fn phone_e164(phone: &str, country: &str) -> Option<String> {
    let p = phone.trim();
    let digits: String = p.chars().filter(char::is_ascii_digit).collect();
    let full = if p.starts_with('+') {
        digits
    } else if let Some(rest) = digits.strip_prefix("00") {
        rest.to_owned()
    } else {
        let national = digits.trim_start_matches('0');
        format!("{}{national}", calling_code(country)?)
    };
    ((8..=15).contains(&full.len()) && !full.starts_with('0')).then(|| format!("+{full}"))
}

/// Meta: the E.164 digits without `+`.
pub fn phone_meta(phone: &str, country: &str) -> Option<String> {
    phone_e164(phone, country).map(|p| p[1..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden vectors from Meta's customer information parameters documentation.
    #[test]
    fn meta_documented_vectors() {
        let em = email_basic("  John_Smith@gmail.com ").unwrap();
        assert_eq!(em, "john_smith@gmail.com");
        assert_eq!(
            sha256_hex(&em),
            "62a14e44f765419d10fea99367361a727c12365e2520f32218d505ed9aa0f62f"
        );
        // Meta's example is a US number; the platform's table covers European markets, so
        // the international form is used here.
        let ph = phone_meta("+1 (650) 555-1212", "CZ").unwrap();
        assert_eq!(ph, "16505551212");
        assert_eq!(
            sha256_hex(&ph),
            "e323ec626319ca94ee8bff2e4c87cf613be6ea19919ed1364124e16807ab3176"
        );
    }

    // Examples from Google's Data Manager API formatting guide.
    #[test]
    fn google_documented_examples() {
        assert_eq!(
            email_google("cloudy.sanfrancisco+shopping@gmail.com").as_deref(),
            Some("cloudysanfrancisco@gmail.com")
        );
        assert_eq!(
            email_google("user.name+NYC@Example.com").as_deref(),
            Some("user.name+nyc@example.com")
        );
        assert_eq!(
            email_google(" a.b @googlemail.com").as_deref(),
            Some("ab@googlemail.com")
        );
        assert_eq!(
            phone_e164("+1 (800) 555-0100", "CZ").as_deref(),
            Some("+18005550100")
        );
        assert_eq!(
            sha256_hex("+18005550100"),
            "fb4f73a6ec5fdb7077d564cdd22c3554b43ce49168550c3b12c547b78c517b30"
        );
    }

    // Seznam SEM user data example (`+420606666666`).
    #[test]
    fn seznam_example_and_national_numbers() {
        assert_eq!(
            phone_e164("606 666 666", "CZ").as_deref(),
            Some("+420606666666")
        );
        assert_eq!(
            phone_e164("00420 606-666-666", "SK").as_deref(),
            Some("+420606666666")
        );
        assert_eq!(
            phone_e164("0905 123 456", "SK").as_deref(),
            Some("+421905123456")
        );
        assert_eq!(
            sha256_hex("+420606666666"),
            "ca6faa7abf120a739e8ce3bcf581553af9ad983b3290ed8cdf8cc40b9d4fefcf"
        );
        assert_eq!(
            email_basic(" Jan.Novak@Email.cz").as_deref(),
            Some("jan.novak@email.cz")
        );
    }

    #[test]
    fn rejects_what_cannot_match() {
        assert_eq!(phone_e164("123", "CZ"), None);
        assert_eq!(phone_e164("606 666 666", "US"), None, "unknown country");
        assert_eq!(phone_e164("+0 123 456 789", "CZ"), None);
        assert_eq!(email_basic("no-at-sign"), None);
        assert_eq!(email_basic("a@localhost"), None);
        assert_eq!(email_google("+x@gmail.com"), None);
    }
}
