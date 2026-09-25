//! Tenant branding for emails: the shop name, the logo (§11.4, WP18), colours from the active
//! theme's design tokens (A6) and the tenant's own subject/intro texts per template and locale.
//! Email clients do not understand `oklch()`, so token colours are converted to hex.

use std::collections::BTreeMap;

use platform::Error;
use platform::db::TenantTx;
use serde::Serialize;
use serde_json::Value;

use crate::themes;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Colors {
    pub background: String,
    pub card: String,
    pub ink: String,
    pub muted: String,
    pub accent: String,
    /// Text on `accent` (white or near-black, whichever reads better).
    pub accent_ink: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: "#f4f5f7".into(),
            card: "#ffffff".into(),
            ink: "#1b1f2a".into(),
            muted: "#5c6370".into(),
            accent: "#2b5aa8".into(),
            accent_ink: "#ffffff".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Brand {
    pub shop_name: String,
    /// The shop's public URL (`https://shop.example`).
    pub shop_url: String,
    pub colors: Colors,
    /// Absolute URL of the tenant's email logo (a PNG/JPEG variant served by the shop origin);
    /// `None` = the shop name is the wordmark.
    pub logo_url: Option<String>,
    /// Tenant texts: `"<locale>:<template>.<subject|intro>"` → text (templates only see them
    /// through `t()` and the subject).
    #[serde(skip)]
    pub texts: BTreeMap<String, String>,
}

/// Widest logo variant an email uses (displayed at up to 160 px, 2x for sharp screens).
const LOGO_MAX_WIDTH: i64 = 320;

impl Brand {
    /// The tenant's name, logo, texts and the colours of its active theme.
    pub async fn load(tx: &mut TenantTx, shop_url: String) -> Result<Self, Error> {
        let shop_name = sqlx::query_scalar!(
            "SELECT name FROM platform.tenants WHERE id = $1",
            tx.tenant_id()
        )
        .fetch_one(&mut **tx)
        .await?;
        let tokens = themes::active_tokens(tx).await?;
        // The largest PNG/JPEG variant up to LOGO_MAX_WIDTH (else the smallest one).
        let logo = sqlx::query_scalar!(
            r#"SELECT v->>'key' AS "key!"
               FROM email_settings s
               JOIN assets a ON a.id = s.logo_asset_id AND a.status = 'ready'
               CROSS JOIN LATERAL jsonb_array_elements(a.variants) v
               WHERE v->>'format' IN ('png', 'jpeg') AND v->>'key' IS NOT NULL
               ORDER BY ((v->>'width')::bigint <= $1) DESC,
                        CASE WHEN (v->>'width')::bigint <= $1 THEN -(v->>'width')::bigint
                             ELSE (v->>'width')::bigint END
               LIMIT 1"#,
            LOGO_MAX_WIDTH
        )
        .fetch_optional(&mut **tx)
        .await?;
        let texts =
            sqlx::query!("SELECT template, locale, subject, intro FROM email_template_texts")
                .fetch_all(&mut **tx)
                .await?
                .into_iter()
                .flat_map(|r| {
                    [("subject", r.subject), ("intro", r.intro)]
                        .into_iter()
                        .filter_map(move |(field, text)| {
                            text.map(|t| (format!("{}:{}.{field}", r.locale, r.template), t))
                        })
                })
                .collect();
        let base = shop_url.trim_end_matches('/').to_owned();
        Ok(Self {
            shop_name,
            logo_url: logo.map(|key| format!("{base}/{key}")),
            shop_url,
            colors: tokens.as_ref().map(colors).unwrap_or_default(),
            texts,
        })
    }

    /// The tenant's text for `key` (`order_confirmation.subject`) in `locale`, if it set one.
    pub fn text(&self, locale: &str, key: &str) -> Option<&str> {
        let lang = locale.split('-').next().unwrap_or(locale);
        self.texts.get(&format!("{lang}:{key}")).map(String::as_str)
    }
}

/// Colours from theme tokens (`colors.background`, `foreground`, `muted-foreground`,
/// `identity`); anything missing or unparseable keeps the platform default.
pub fn colors(tokens: &Value) -> Colors {
    let get = |key: &str, fallback: String| {
        tokens
            .pointer(&format!("/colors/{key}"))
            .and_then(Value::as_str)
            .and_then(to_hex)
            .unwrap_or(fallback)
    };
    let d = Colors::default();
    let accent = get("identity", d.accent);
    Colors {
        background: get("background", d.background),
        card: get("card", d.card),
        ink: get("foreground", d.ink),
        muted: get("muted-foreground", d.muted),
        accent_ink: if luminance(&accent) > 0.4 {
            "#111111".into()
        } else {
            "#ffffff".into()
        },
        accent,
    }
}

/// `#rrggbb` (as is) or `oklch(L C H)` (L as 0-1 or percent) to `#rrggbb`.
pub fn to_hex(color: &str) -> Option<String> {
    let c = color.trim();
    if c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(c.to_ascii_lowercase());
    }
    let inner = c.strip_prefix("oklch(")?.strip_suffix(')')?;
    let mut parts = inner.split_whitespace();
    let l = parts.next()?;
    let l: f64 = match l.strip_suffix('%') {
        Some(p) => p.parse::<f64>().ok()? / 100.0,
        None => l.parse().ok()?,
    };
    let chroma: f64 = parts.next()?.parse().ok()?;
    let hue: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let (a, b) = (
        chroma * hue.to_radians().cos(),
        chroma * hue.to_radians().sin(),
    );
    // OKLab → linear sRGB (Björn Ottosson's reference matrices).
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548 * b).powi(3);
    let rgb = [
        4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_,
        -1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701 * s_,
    ];
    let byte = |x: f64| {
        let x = x.clamp(0.0, 1.0);
        let srgb = if x <= 0.003_130_8 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
        // Clamped to 0..=255 above, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = (srgb * 255.0).round() as u8;
        v
    };
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        byte(rgb[0]),
        byte(rgb[1]),
        byte(rgb[2])
    ))
}

/// Relative luminance (WCAG) of `#rrggbb`.
fn luminance(hex: &str) -> f64 {
    let channel = |i: usize| {
        let v = f64::from(u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0))
            / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oklch_converts_to_srgb_hex() {
        assert_eq!(to_hex("oklch(1 0 0)").as_deref(), Some("#ffffff"));
        assert_eq!(to_hex("oklch(0 0 0)").as_deref(), Some("#000000"));
        assert_eq!(
            to_hex("oklch(62.8% 0.2577 29.23)").as_deref(),
            Some("#ff0000")
        );
        assert_eq!(to_hex("#1F3A2E").as_deref(), Some("#1f3a2e"));
        assert_eq!(to_hex("red"), None);
        assert_eq!(to_hex("oklch(0.5 0.1)"), None);
        assert_eq!(to_hex("oklch(0.5 0.1 20 4)"), None);
    }

    #[test]
    fn colors_come_from_tokens_with_readable_button_text() {
        let c = colors(&json!({"colors": {
            "identity": "oklch(0.49 0.135 255)",
            "background": "oklch(0.975 0.003 250)",
            "foreground": "not a colour"
        }}));
        assert!(c.accent.starts_with('#') && c.accent != Colors::default().accent);
        assert_eq!(c.accent_ink, "#ffffff");
        assert_eq!(c.ink, Colors::default().ink);
        let light = colors(&json!({"colors": {"identity": "#f0b14a"}}));
        assert_eq!(light.accent_ink, "#111111");
    }
}
