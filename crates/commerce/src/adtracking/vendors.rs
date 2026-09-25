//! Vendor request shapes. Pure functions: an event (with the order read at send time) plus
//! the platform's settings and credentials in, one HTTP request out. Identifiers are hashed
//! here, in memory; nothing built here is stored.
//!
//! - Meta Conversions API (Graph API v26.0): `POST /{pixel}/events`, `action_source=website`,
//!   `event_id` for deduplication, `user_data` with hashed `em`/`ph`/`country`/`external_id`
//!   and the (unhashed) `client_user_agent` that website events require. Test mode sends the
//!   configured `test_event_code` (Events Manager → Test events).
//! - GA4 Measurement Protocol: `POST /mp/collect?measurement_id&api_secret` on the EU
//!   endpoint (`region1`), a pseudonymous `client_id`, no PII. Test mode uses the validation
//!   server (`/debug/mp/collect`), which records nothing.
//! - Google Ads via the Data Manager API (`POST /v1/events:ingest`, OAuth 2 refresh token →
//!   access token; the Google Ads API `UploadClickConversions` route is deprecated for
//!   offline and enhanced conversions for leads since 2026-06-15): purchases only, hashed
//!   email/phone as `userIdentifiers` (`encoding: HEX`). Test mode sets `validateOnly`.
//! - Seznam SEM server-to-server (`POST https://sem.seznam.cz/rtgconv`, schema v2):
//!   purchases only, CZK only, hashed `em`/`ph`. Seznam attributes S2S events through the
//!   `sid`/`udid` cookies of its browser script (`sul.js`), which the platform does not load
//!   (no third-party scripts, §11.3), so matching relies on the hashed identifiers. Seznam
//!   offers no test channel: test mode sends nothing.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use super::normalize::{email_basic, email_google, phone_e164, phone_meta, sha256_hex};
use super::{Credentials, Platform, Settings};
use crate::money::Currency;

pub const META_VERSION: &str = "v26.0";
const META_HOST: &str = "https://graph.facebook.com";
const GA4_HOST: &str = "https://region1.google-analytics.com";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_INGEST_URL: &str = "https://datamanager.googleapis.com/v1/events:ingest";
const SKLIK_URL: &str = "https://sem.seznam.cz/rtgconv";

/// Where requests go: the vendors, or (dev only) the local mocks under one base URL
/// (`<base>/meta/...`, `<base>/ga4/...`, `<base>/google/...`, `<base>/sklik/...`).
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    pub base: Option<String>,
}

impl Endpoints {
    fn at(&self, platform: &str, real: &str, path: &str) -> String {
        match &self.base {
            Some(b) => format!("{}/{platform}{path}", b.trim_end_matches('/')),
            None => format!("{real}{path}"),
        }
    }
    pub fn meta_events(&self, pixel: &str) -> String {
        self.at(
            "meta",
            META_HOST,
            &format!("/{META_VERSION}/{pixel}/events"),
        )
    }
    pub fn meta_pixel(&self, pixel: &str) -> String {
        self.at("meta", META_HOST, &format!("/{META_VERSION}/{pixel}"))
    }
    pub fn ga4(&self, debug: bool) -> String {
        let path = if debug {
            "/debug/mp/collect"
        } else {
            "/mp/collect"
        };
        self.at("ga4", GA4_HOST, path)
    }
    pub fn google_token(&self) -> String {
        match &self.base {
            Some(_) => self.at("google", "", "/token"),
            None => GOOGLE_TOKEN_URL.to_owned(),
        }
    }
    pub fn google_ingest(&self) -> String {
        match &self.base {
            Some(_) => self.at("google", "", "/v1/events:ingest"),
            None => GOOGLE_INGEST_URL.to_owned(),
        }
    }
    pub fn sklik(&self) -> String {
        match &self.base {
            Some(_) => self.at("sklik", "", "/rtgconv"),
            None => SKLIK_URL.to_owned(),
        }
    }
}

/// One order line as the vendors see it.
#[derive(Debug, Clone)]
pub struct Line {
    pub sku: String,
    pub name: String,
    pub quantity: i32,
    pub unit_gross_minor: i64,
    /// The line after discounts, without VAT.
    pub net_minor: i64,
}

/// The order behind a purchase or refund, read when sending.
#[derive(Debug, Clone)]
pub struct Order {
    pub number: i64,
    pub email: String,
    pub phone: Option<String>,
    pub country: String,
    pub currency: String,
    pub total_minor: i64,
    pub tax_minor: i64,
    pub shipping_minor: i64,
    pub lines: Vec<Line>,
}

/// Everything a request is built from.
#[derive(Debug, Clone)]
pub struct Event {
    pub event_name: String,
    pub event_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    /// The page (or the shop's home page for server events).
    pub url: String,
    /// Tenant-scoped hash of the consent subject: GA4 `client_id`, Meta `external_id`.
    pub pseudonym: String,
    pub user_agent: Option<String>,
    /// `skus` (catalog ids as in the export feeds), `quantity`; refunds: `amount_minor`.
    pub props: Value,
    pub order: Option<Order>,
    pub test_mode: bool,
}

/// A ready request: POST `body` as JSON to `url`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub url: String,
    pub bearer: Option<String>,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

fn major(minor: i64, currency: &str) -> f64 {
    let exp = Currency::parse(currency).map_or(2, Currency::exponent);
    #[allow(clippy::cast_precision_loss)]
    let v = minor as f64 / 10f64.powi(i32::try_from(exp).unwrap_or(2));
    v
}

fn skus(e: &Event) -> Vec<String> {
    match &e.order {
        Some(o) => o.lines.iter().map(|l| l.sku.clone()).collect(),
        None => e.props["skus"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn quantity(e: &Event) -> i64 {
    e.props["quantity"].as_i64().unwrap_or(1)
}

/// The vendor's event name, `None` when the platform does not take this event.
pub fn event_name(platform: Platform, ours: &str) -> Option<&'static str> {
    Some(match (platform, ours) {
        (Platform::Meta, "page_view") => "PageView",
        (Platform::Meta, "view_item") => "ViewContent",
        (Platform::Meta, "add_to_cart") => "AddToCart",
        (Platform::Meta, "begin_checkout") => "InitiateCheckout",
        (Platform::Meta, "purchase") => "Purchase",
        (Platform::Ga4, "page_view") => "page_view",
        (Platform::Ga4, "view_item") => "view_item",
        (Platform::Ga4, "add_to_cart") => "add_to_cart",
        (Platform::Ga4, "begin_checkout") => "begin_checkout",
        (Platform::Ga4, "purchase") => "purchase",
        (Platform::Ga4, "refund") => "refund",
        (Platform::GoogleAds, "purchase") => "purchase",
        (Platform::Sklik, "purchase") => "Purchase",
        _ => return None,
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("{0} is not configured")]
    Missing(&'static str),
    #[error("the platform does not take {0} events")]
    Unsupported(String),
    #[error("the order has no identifier the platform can match")]
    NoIdentifiers,
}

fn need<'a>(v: Option<&'a String>, what: &'static str) -> Result<&'a str, BuildError> {
    v.map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(BuildError::Missing(what))
}

pub fn build(
    endpoints: &Endpoints,
    platform: Platform,
    settings: &Settings,
    creds: &Credentials,
    e: &Event,
) -> Result<Request, BuildError> {
    let name = event_name(platform, &e.event_name)
        .ok_or_else(|| BuildError::Unsupported(e.event_name.clone()))?;
    match platform {
        Platform::Meta => meta(endpoints, settings, creds, e, name),
        Platform::Ga4 => ga4(endpoints, settings, creds, e, name),
        Platform::GoogleAds => google_ads(endpoints, settings, e),
        Platform::Sklik => sklik(endpoints, settings, e),
    }
}

fn meta(
    endpoints: &Endpoints,
    settings: &Settings,
    creds: &Credentials,
    e: &Event,
    name: &str,
) -> Result<Request, BuildError> {
    let pixel = need(settings.pixel_id.as_ref(), "pixel_id")?;
    let token = need(creds.access_token.as_ref(), "access_token")?;
    let mut user = json!({ "external_id": [sha256_hex(&e.pseudonym)] });
    if let Some(ua) = &e.user_agent {
        user["client_user_agent"] = json!(ua);
    }
    let mut custom = json!({});
    let ids = skus(e);
    if !ids.is_empty() {
        custom["content_ids"] = json!(ids);
        custom["content_type"] = json!("product");
    }
    if let Some(o) = &e.order {
        if let Some(em) = email_basic(&o.email) {
            user["em"] = json!([sha256_hex(&em)]);
        }
        if let Some(ph) = o.phone.as_deref().and_then(|p| phone_meta(p, &o.country)) {
            user["ph"] = json!([sha256_hex(&ph)]);
        }
        user["country"] = json!([sha256_hex(&o.country.to_lowercase())]);
        custom["currency"] = json!(o.currency);
        custom["value"] = json!(major(o.total_minor, &o.currency));
        custom["order_id"] = json!(o.number.to_string());
        custom["num_items"] = json!(o.lines.iter().map(|l| i64::from(l.quantity)).sum::<i64>());
        custom["contents"] = json!(
            o.lines
                .iter()
                .map(|l| json!({ "id": l.sku, "quantity": l.quantity,
                                 "item_price": major(l.unit_gross_minor, &o.currency) }))
                .collect::<Vec<_>>()
        );
    } else if e.event_name == "add_to_cart" {
        custom["contents"] = json!(
            ids.iter()
                .map(|id| json!({ "id": id, "quantity": quantity(e) }))
                .collect::<Vec<_>>()
        );
    }
    let mut event = json!({
        "event_name": name,
        "event_time": e.occurred_at.timestamp(),
        "event_id": e.event_id.to_string(),
        "action_source": "website",
        "event_source_url": e.url,
        "user_data": user,
    });
    if custom.as_object().is_some_and(|m| !m.is_empty()) {
        event["custom_data"] = custom;
    }
    let mut body = json!({ "data": [event], "access_token": token });
    if e.test_mode {
        body["test_event_code"] =
            json!(need(settings.test_event_code.as_ref(), "test_event_code")?);
    }
    Ok(Request {
        url: endpoints.meta_events(pixel),
        bearer: None,
        headers: vec![],
        body,
    })
}

fn ga4(
    endpoints: &Endpoints,
    settings: &Settings,
    creds: &Credentials,
    e: &Event,
    name: &str,
) -> Result<Request, BuildError> {
    let measurement = need(settings.measurement_id.as_ref(), "measurement_id")?;
    let secret = need(creds.api_secret.as_ref(), "api_secret")?;
    let mut params = json!({ "engagement_time_msec": 1 });
    match &e.order {
        Some(o) => {
            params["transaction_id"] = json!(o.number.to_string());
            params["currency"] = json!(o.currency);
            if e.event_name == "purchase" {
                // GA4: `value` is the merchandise (items after discounts, without VAT and
                // shipping); tax and shipping go separately, item prices match the value.
                let merchandise: i64 = o.lines.iter().map(|l| l.net_minor).sum();
                params["value"] = json!(major(merchandise, &o.currency));
                params["tax"] = json!(major(o.tax_minor, &o.currency));
                params["shipping"] = json!(major(o.shipping_minor, &o.currency));
                params["items"] = json!(
                    o.lines
                        .iter()
                        .map(|l| json!({ "item_id": l.sku, "item_name": l.name,
                                         "price": major(l.net_minor, &o.currency)
                                             / f64::from(l.quantity.max(1)),
                                         "quantity": l.quantity }))
                        .collect::<Vec<_>>()
                );
            } else {
                // A refund: the refunded amount (the whole order when not given).
                let value = e.props["amount_minor"].as_i64().unwrap_or(o.total_minor);
                params["value"] = json!(major(value, &o.currency));
            }
        }
        None => {
            params["page_location"] = json!(e.url);
            let ids = skus(e);
            if !ids.is_empty() {
                let q = quantity(e);
                params["items"] = json!(
                    ids.iter()
                        .map(|id| json!({ "item_id": id, "quantity": q }))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    let url = format!(
        "{}?measurement_id={}&api_secret={}",
        endpoints.ga4(e.test_mode),
        query_escape(measurement),
        query_escape(secret)
    );
    // GA4 has no PII: `client_id` is the pseudonym (no email, no IP, no user agent).
    Ok(Request {
        url,
        bearer: None,
        headers: vec![],
        body: json!({
            "client_id": &e.pseudonym[..32.min(e.pseudonym.len())],
            "timestamp_micros": e.occurred_at.timestamp_micros(),
            "consent": { "ad_user_data": "GRANTED", "ad_personalization": "GRANTED" },
            "events": [{ "name": name, "params": params }],
        }),
    })
}

/// `application/x-www-form-urlencoded` encoding of one value.
pub fn query_escape(v: &str) -> String {
    let mut u = reqwest::Url::parse("http://x/").unwrap_or_else(|_| unreachable!("a valid URL"));
    u.query_pairs_mut().append_pair("v", v);
    u.query()
        .unwrap_or_default()
        .trim_start_matches("v=")
        .to_owned()
}

fn google_ads(
    endpoints: &Endpoints,
    settings: &Settings,
    e: &Event,
) -> Result<Request, BuildError> {
    let customer = need(settings.customer_id.as_ref(), "customer_id")?;
    let action = need(
        settings.conversion_action_id.as_ref(),
        "conversion_action_id",
    )?;
    let login = settings
        .login_customer_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(customer);
    let o = e.order.as_ref().ok_or(BuildError::NoIdentifiers)?;
    let mut ids = Vec::new();
    if let Some(em) = email_google(&o.email) {
        ids.push(json!({ "emailAddress": sha256_hex(&em) }));
    }
    if let Some(ph) = o.phone.as_deref().and_then(|p| phone_e164(p, &o.country)) {
        ids.push(json!({ "phoneNumber": sha256_hex(&ph) }));
    }
    if ids.is_empty() {
        return Err(BuildError::NoIdentifiers);
    }
    Ok(Request {
        url: endpoints.google_ingest(),
        // The access token is added when sending (it is exchanged for the refresh token).
        bearer: None,
        headers: vec![],
        body: json!({
            "destinations": [{
                "operatingAccount": { "accountType": "GOOGLE_ADS", "accountId": customer },
                "loginAccount": { "accountType": "GOOGLE_ADS", "accountId": login },
                "productDestinationId": action,
            }],
            "encoding": "HEX",
            "consent": { "adUserData": "CONSENT_GRANTED", "adPersonalization": "CONSENT_GRANTED" },
            "events": [{
                "transactionId": o.number.to_string(),
                "eventTimestamp": e.occurred_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                "conversionValue": major(o.total_minor, &o.currency),
                "currency": o.currency,
                "eventSource": "WEB",
                "userData": { "userIdentifiers": ids },
            }],
            "validateOnly": e.test_mode,
        }),
    })
}

fn sklik(endpoints: &Endpoints, settings: &Settings, e: &Event) -> Result<Request, BuildError> {
    let sem = need(settings.sem_id.as_ref(), "sem_id")?;
    let o = e.order.as_ref().ok_or(BuildError::NoIdentifiers)?;
    let mut user = json!({});
    if let Some(em) = email_basic(&o.email) {
        user["em"] = json!(sha256_hex(&em));
    }
    if let Some(ph) = o.phone.as_deref().and_then(|p| phone_e164(p, &o.country)) {
        user["ph"] = json!(sha256_hex(&ph));
    }
    Ok(Request {
        url: endpoints.sklik(),
        bearer: None,
        headers: vec![
            ("x-client-id", "commerce-platform".to_owned()),
            ("x-client-version", "1".to_owned()),
        ],
        body: json!({
            "schema_version": "v2",
            "event_name": "Purchase",
            "event_type": "rtgconv",
            "event_time": e.occurred_at.timestamp_millis(),
            "event_url": e.url,
            "event_source": "web",
            "event_id": e.event_id.to_string(),
            "user_ids": { "user_data": user },
            "consent_mode": { "ad_user_data": "granted", "ad_personalization": "granted" },
            "event_data": {
                "sem_id": sem,
                "order_id": o.number.to_string(),
                "currency": o.currency,
                // SEM wants the value without VAT and the VAT separately.
                "value": major(o.total_minor - o.tax_minor, &o.currency),
                "value_tax": major(o.tax_minor, &o.currency),
                "content_type": "product",
                "contents": o.lines.iter().map(|l| json!({
                    "id": l.sku, "quantity": l.quantity, "content_name": l.name,
                    "unit_price": major(l.unit_gross_minor, &o.currency),
                })).collect::<Vec<_>>(),
            },
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> Order {
        Order {
            number: 1001,
            email: " Jan.Novak+eshop@Gmail.com ".into(),
            phone: Some("606 666 666".into()),
            country: "CZ".into(),
            currency: "CZK".into(),
            total_minor: 25_800,
            tax_minor: 4_478,
            shipping_minor: 0,
            lines: vec![Line {
                sku: "TEE-M".into(),
                name: "Tričko".into(),
                quantity: 2,
                unit_gross_minor: 12_900,
                net_minor: 21_322,
            }],
        }
    }

    fn purchase() -> Event {
        Event {
            event_name: "purchase".into(),
            event_id: Uuid::from_u128(7),
            occurred_at: DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
            url: "https://shop.example/".into(),
            pseudonym: "ab".repeat(32),
            user_agent: Some("Mozilla/5.0".into()),
            props: json!({}),
            order: Some(order()),
            test_mode: false,
        }
    }

    fn settings() -> Settings {
        Settings {
            pixel_id: Some("123456789".into()),
            measurement_id: Some("G-ABC123".into()),
            customer_id: Some("1234567890".into()),
            conversion_action_id: Some("987654321".into()),
            sem_id: Some("sem-s2s-1".into()),
            ..Settings::default()
        }
    }

    fn creds() -> Credentials {
        Credentials {
            access_token: Some("EAAtoken".into()),
            api_secret: Some("s3cr&t".into()),
            ..Credentials::default()
        }
    }

    #[test]
    fn meta_purchase_is_hashed_and_deduplicable() {
        let r = build(
            &Endpoints::default(),
            Platform::Meta,
            &settings(),
            &creds(),
            &purchase(),
        )
        .unwrap();
        assert_eq!(r.url, "https://graph.facebook.com/v26.0/123456789/events");
        let ev = &r.body["data"][0];
        assert_eq!(ev["event_name"], "Purchase");
        assert_eq!(ev["action_source"], "website");
        assert_eq!(ev["event_id"], Uuid::from_u128(7).to_string());
        assert_eq!(ev["event_time"], 1_790_000_000);
        // Meta: trimmed + lowercased, the Gmail dots and +suffix stay.
        assert_eq!(
            ev["user_data"]["em"][0],
            sha256_hex("jan.novak+eshop@gmail.com")
        );
        assert_eq!(ev["user_data"]["ph"][0], sha256_hex("420606666666"));
        assert_eq!(ev["user_data"]["country"][0], sha256_hex("cz"));
        assert_eq!(ev["user_data"]["client_user_agent"], "Mozilla/5.0");
        assert_eq!(ev["custom_data"]["value"], 258.0);
        assert_eq!(ev["custom_data"]["currency"], "CZK");
        assert_eq!(ev["custom_data"]["order_id"], "1001");
        assert_eq!(ev["custom_data"]["content_ids"], json!(["TEE-M"]));
        assert_eq!(r.body["access_token"], "EAAtoken");
        assert!(r.body.get("test_event_code").is_none());
        let raw = r.body.to_string();
        assert!(!raw.contains("novak") && !raw.contains("606"), "{raw}");
    }

    #[test]
    fn meta_test_mode_needs_a_test_event_code() {
        let mut e = purchase();
        e.test_mode = true;
        assert_eq!(
            build(
                &Endpoints::default(),
                Platform::Meta,
                &settings(),
                &creds(),
                &e
            ),
            Err(BuildError::Missing("test_event_code"))
        );
        let s = Settings {
            test_event_code: Some("TEST123".into()),
            ..settings()
        };
        let r = build(&Endpoints::default(), Platform::Meta, &s, &creds(), &e).unwrap();
        assert_eq!(r.body["test_event_code"], "TEST123");
    }

    #[test]
    fn ga4_carries_no_pii_and_uses_the_eu_endpoint() {
        let r = build(
            &Endpoints::default(),
            Platform::Ga4,
            &settings(),
            &creds(),
            &purchase(),
        )
        .unwrap();
        assert_eq!(
            r.url,
            "https://region1.google-analytics.com/mp/collect?measurement_id=G-ABC123&api_secret=s3cr%26t"
        );
        assert_eq!(r.body["client_id"], "ab".repeat(16));
        let ev = &r.body["events"][0];
        assert_eq!(ev["name"], "purchase");
        assert_eq!(ev["params"]["transaction_id"], "1001");
        assert_eq!(ev["params"]["items"][0]["item_id"], "TEE-M");
        // Merchandise without VAT and shipping; item prices add up to it.
        assert_eq!(ev["params"]["value"], 213.22);
        assert_eq!(ev["params"]["items"][0]["price"], 106.61);
        assert_eq!(ev["params"]["tax"], 44.78);
        let raw = r.body.to_string();
        assert!(!raw.contains('@') && !raw.contains("Mozilla"), "{raw}");

        let mut view = purchase();
        view.order = None;
        view.event_name = "view_item".into();
        view.test_mode = true;
        view.props = json!({ "skus": ["TEE-M", "TEE-L"] });
        let r = build(
            &Endpoints::default(),
            Platform::Ga4,
            &settings(),
            &creds(),
            &view,
        )
        .unwrap();
        assert!(
            r.url
                .starts_with("https://region1.google-analytics.com/debug/mp/collect?")
        );
        assert_eq!(
            r.body["events"][0]["params"]["items"][1]["item_id"],
            "TEE-L"
        );
    }

    #[test]
    fn google_ads_uses_google_normalization() {
        let r = build(
            &Endpoints::default(),
            Platform::GoogleAds,
            &settings(),
            &creds(),
            &purchase(),
        )
        .unwrap();
        assert_eq!(r.url, "https://datamanager.googleapis.com/v1/events:ingest");
        let ev = &r.body["events"][0];
        // Gmail: dots and +suffix removed before hashing.
        assert_eq!(
            ev["userData"]["userIdentifiers"][0]["emailAddress"],
            sha256_hex("jannovak@gmail.com")
        );
        assert_eq!(
            ev["userData"]["userIdentifiers"][1]["phoneNumber"],
            sha256_hex("+420606666666")
        );
        assert_eq!(ev["transactionId"], "1001");
        assert_eq!(ev["eventTimestamp"], "2026-09-21T14:13:20Z");
        assert_eq!(
            r.body["destinations"][0]["productDestinationId"],
            "987654321"
        );
        assert_eq!(r.body["encoding"], "HEX");
        assert_eq!(r.body["validateOnly"], false);
        let mut view = purchase();
        view.event_name = "view_item".into();
        assert!(matches!(
            build(
                &Endpoints::default(),
                Platform::GoogleAds,
                &settings(),
                &creds(),
                &view
            ),
            Err(BuildError::Unsupported(_))
        ));
    }

    #[test]
    fn sklik_sends_net_value_and_e164_phone() {
        let r = build(
            &Endpoints {
                base: Some("http://mocks:4010/ads/".into()),
            },
            Platform::Sklik,
            &settings(),
            &creds(),
            &purchase(),
        )
        .unwrap();
        assert_eq!(r.url, "http://mocks:4010/ads/sklik/rtgconv");
        assert_eq!(r.body["schema_version"], "v2");
        assert_eq!(r.body["event_type"], "rtgconv");
        assert_eq!(r.body["event_time"], 1_790_000_000_000_i64);
        assert_eq!(r.body["event_data"]["sem_id"], "sem-s2s-1");
        assert_eq!(r.body["event_data"]["value"], 213.22);
        assert_eq!(r.body["event_data"]["value_tax"], 44.78);
        assert_eq!(
            r.body["user_ids"]["user_data"]["ph"],
            sha256_hex("+420606666666")
        );
        assert_eq!(
            r.body["user_ids"]["user_data"]["em"],
            sha256_hex("jan.novak+eshop@gmail.com")
        );
    }

    #[test]
    fn mock_endpoints_mirror_the_vendor_paths() {
        let m = Endpoints {
            base: Some("http://mocks:4010/ads".into()),
        };
        assert_eq!(
            m.meta_events("1"),
            "http://mocks:4010/ads/meta/v26.0/1/events"
        );
        assert_eq!(m.ga4(true), "http://mocks:4010/ads/ga4/debug/mp/collect");
        assert_eq!(m.google_token(), "http://mocks:4010/ads/google/token");
        assert_eq!(
            m.google_ingest(),
            "http://mocks:4010/ads/google/v1/events:ingest"
        );
    }
}
