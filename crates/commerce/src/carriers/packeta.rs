//! Packeta (Zásilkovna) REST/XML API (`POST https://www.zasilkovna.cz/api/rest`): the root
//! element names the method, `<apiPassword>` authenticates; answers are
//! `<response><status>ok|fault</status><result>…</result>…</response>` with HTTP 200 either way.
//!
//! - `createPacket`: `addressId` = the pickup point, or the home-delivery carrier (CZ 106,
//!   SK 131) with the address; `cod` for cash on delivery. Result `id` (packet id, our
//!   `carrier_ref`) and `barcode` (`Z…`, the tracking number).
//! - `packetLabelPdf` (`A6 on A6`): base64 PDF.
//! - `packetStatus`: `statusCode` (1 data received … 7 delivered, 9/10 returning/returned,
//!   11 cancelled).
//! - Pickup-point validation (widget API): `POST …/v1/validate` `{apiKey, point: {id}}` →
//!   `{isValid, errors}`.

use base64::Engine;
use platform::Error;
use quick_xml::escape::escape;
use serde::Deserialize;
use serde_json::json;

use super::{Carriers, Created, MAX_LABEL_BYTES, ShipmentRequest, TIMEOUT, Tracked, decimal};
use crate::shipping::Carrier;

const NAME: &str = "Packeta";

/// Packeta home-delivery carriers ("Zásilkovna domů") by destination country.
fn home_carrier(country: &str) -> Option<&'static str> {
    match country {
        "CZ" => Some("106"),
        "SK" => Some("131"),
        _ => None,
    }
}

/// The text of the first element named `tag` in `xml` (entities resolved). DOCTYPEs are
/// refused (no entity expansion from the response).
fn text_of(xml: &str, tag: &str) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut text: Option<String> = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) if text.is_none() && e.name().as_ref() == tag.as_bytes() => {
                text = Some(String::new());
            }
            Event::Text(t) => {
                if let Some(buf) = text.as_mut() {
                    buf.push_str(&t.decode().ok()?);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(buf) = text.as_mut() {
                    match r.resolve_char_ref().ok()? {
                        Some(c) => buf.push(c),
                        None => buf.push_str(match r.decode().ok()?.as_ref() {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "quot" => "\"",
                            "apos" => "'",
                            _ => return None,
                        }),
                    }
                }
            }
            Event::End(e) if text.is_some() && e.name().as_ref() == tag.as_bytes() => {
                return text;
            }
            Event::DocType(_) | Event::Eof => return None,
            _ => {}
        }
    }
}

fn el(name: &str, value: &str) -> String {
    format!("<{name}>{}</{name}>", escape(value))
}

async fn call(c: &Carriers, body: String) -> Result<String, Error> {
    let res = c
        .http
        .post(&c.packeta_url)
        .header("content-type", "text/xml; charset=utf-8")
        .body(body)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    if !res.status().is_success() {
        return Err(super::unavailable(NAME, format!("HTTP {}", res.status())));
    }
    let text = res.text().await.map_err(|e| super::unavailable(NAME, e))?;
    match text_of(&text, "status").as_deref() {
        Some("ok") => Ok(text),
        Some("fault") => {
            let fault = text_of(&text, "fault").unwrap_or_default();
            let detail = text_of(&text, "string").unwrap_or_default();
            // A detail per attribute, when there is one (`addressId: Unknown address id`).
            let attr = text_of(&text, "name")
                .map(|n| format!(" ({n})"))
                .unwrap_or_default();
            Err(super::rejected(NAME, format!("{fault}: {detail}{attr}")))
        }
        _ => Err(super::unavailable(NAME, "unexpected response")),
    }
}

/// Splits "Jana Nováková" into Packeta's name/surname (single words go to both).
fn split_name(full: &str) -> (String, String) {
    let full = full.trim();
    match full.rsplit_once(' ') {
        Some((first, last)) => (first.trim().to_owned(), last.trim().to_owned()),
        None => (full.to_owned(), full.to_owned()),
    }
}

pub(crate) async fn create(
    c: &Carriers,
    password: &str,
    sender: &str,
    method: Carrier,
    req: &ShipmentRequest,
) -> Result<Created, Error> {
    let (name, surname) = split_name(&req.recipient_name);
    let mut attrs = vec![
        el("number", &req.reference),
        el("name", &name),
        el("surname", &surname),
    ];
    if let Some(company) = &req.company {
        attrs.push(el("company", company));
    }
    attrs.push(el("email", &req.email));
    if let Some(phone) = &req.phone {
        attrs.push(el("phone", phone));
    }
    let address_id = match method {
        Carrier::PacketaPickup => req
            .pickup_point
            .clone()
            .ok_or_else(|| super::rejected(NAME, "the order has no pickup point"))?,
        _ => home_carrier(&req.country)
            .ok_or_else(|| super::rejected(NAME, format!("no home delivery to {}", req.country)))?
            .to_owned(),
    };
    attrs.push(el("addressId", &address_id));
    if let Some(cod) = req.cod_minor {
        attrs.push(el("cod", &decimal(cod)));
    }
    attrs.push(el("value", &decimal(req.value_minor)));
    attrs.push(el("currency", &req.currency));
    attrs.push(el(
        "weight",
        &format!("{:.3}", req.weight_g as f64 / 1000.0),
    ));
    attrs.push(el("eshop", sender));
    if method != Carrier::PacketaPickup {
        attrs.push(el("street", &req.street));
        attrs.push(el("city", &req.city));
        attrs.push(el("zip", &req.postal_code));
    }
    let body = format!(
        "<createPacket>{}<packetAttributes>{}</packetAttributes></createPacket>",
        el("apiPassword", password),
        attrs.concat()
    );
    let created = call(c, body).await?;
    let id = text_of(&created, "id")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| super::unavailable(NAME, "no packet id"))?;
    let barcode = text_of(&created, "barcode").unwrap_or_else(|| format!("Z{id}"));
    let label = call(
        c,
        format!(
            "<packetLabelPdf>{}{}<format>A6 on A6</format><offset>0</offset></packetLabelPdf>",
            el("apiPassword", password),
            el("packetId", &id)
        ),
    )
    .await?;
    let b64 = text_of(&label, "result").unwrap_or_default();
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| super::unavailable(NAME, "label is not base64"))?;
    if !pdf.starts_with(b"%PDF") || pdf.len() > MAX_LABEL_BYTES {
        return Err(super::unavailable(NAME, "label is not a PDF"));
    }
    Ok(Created {
        tracking_url: format!("https://tracking.packeta.com/cs/?id={barcode}"),
        carrier_ref: id,
        tracking_number: barcode,
        label_pdf: pdf,
    })
}

/// `statusCode` → our state.
pub(crate) fn map_status(code: u32) -> Tracked {
    match code {
        1 => Tracked::Announced,
        7 => Tracked::Delivered,
        9 | 10 => Tracked::Returned,
        11 => Tracked::Cancelled,
        _ => Tracked::InTransit,
    }
}

pub(crate) async fn track(
    c: &Carriers,
    password: &str,
    packet_id: &str,
) -> Result<(Tracked, String), Error> {
    let res = call(
        c,
        format!(
            "<packetStatus>{}{}</packetStatus>",
            el("apiPassword", password),
            el("packetId", packet_id)
        ),
    )
    .await?;
    let code: u32 = text_of(&res, "statusCode")
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| super::unavailable(NAME, "no statusCode"))?;
    let text = text_of(&res, "codeText").unwrap_or_default();
    Ok((map_status(code), format!("{code} {text}").trim().to_owned()))
}

#[derive(Deserialize)]
struct Validation {
    #[serde(rename = "isValid")]
    is_valid: bool,
}

/// Whether Packeta knows the pickup point (A: the widget's choice is re-checked server-side).
/// `Err` only when the service could not answer.
pub async fn validate_point(c: &Carriers, api_key: &str, point_id: &str) -> Result<bool, Error> {
    let res = c
        .http
        .post(&c.packeta_validate_url)
        .json(&json!({ "apiKey": api_key, "point": { "id": point_id } }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    if !res.status().is_success() {
        return Err(super::unavailable(NAME, format!("HTTP {}", res.status())));
    }
    let v: Validation = res.json().await.map_err(|e| super::unavailable(NAME, e))?;
    Ok(v.is_valid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_results_and_faults() {
        let ok = "<response><status>ok</status><result><id>2000000001</id>\
                  <barcode>Z2000000001</barcode><barcodeText>Z 200</barcodeText></result></response>";
        assert_eq!(text_of(ok, "status").as_deref(), Some("ok"));
        assert_eq!(text_of(ok, "id").as_deref(), Some("2000000001"));
        assert_eq!(text_of(ok, "barcode").as_deref(), Some("Z2000000001"));
        let fault = "<response><status>fault</status><fault>PacketAttributesFault</fault>\
                     <string>Invalid &amp; wrong</string><detail><attributes><fault><name>addressId</name>\
                     <fault>Unknown</fault></fault></attributes></detail></response>";
        assert_eq!(text_of(fault, "string").as_deref(), Some("Invalid & wrong"));
        assert_eq!(text_of(fault, "name").as_deref(), Some("addressId"));
        assert_eq!(text_of("<a><b></b></a>", "b").as_deref(), Some(""));
        assert_eq!(text_of("<a/>", "b"), None);
    }

    #[test]
    fn names_statuses_and_escaping() {
        assert_eq!(
            split_name("Jana Nováková"),
            ("Jana".into(), "Nováková".into())
        );
        assert_eq!(split_name("Cher"), ("Cher".into(), "Cher".into()));
        assert_eq!(el("name", "A&B <x>"), "<name>A&amp;B &lt;x&gt;</name>");
        assert_eq!(map_status(1), Tracked::Announced);
        assert_eq!(map_status(4), Tracked::InTransit);
        assert_eq!(map_status(7), Tracked::Delivered);
        assert_eq!(map_status(10), Tracked::Returned);
        assert_eq!(map_status(11), Tracked::Cancelled);
    }
}
