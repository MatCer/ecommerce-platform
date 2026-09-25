//! PPL CPL API ("MyAPI2", base `https://api.dhl.com/ecs/ppl/myapi2`):
//!
//! - OAuth 2 client credentials: `POST /login/getAccessToken` (`scope=myapi2`); tokens live
//!   30 minutes and are reused (PPL asks not to request one per call).
//! - `POST /shipment/batch` → `201` + `Location: /shipment/batch/{batchId}`; `GET` that until
//!   the item's `importState` is `Complete` with a `labelUrl` (a PDF, fetched with the token).
//!   Products: `PRIV` (B2C), `PRID` (B2C with cash on delivery: `cashOnDelivery`).
//! - Tracking: `GET /shipment?ShipmentNumbers=…` → `trackAndTrace.phase` (`Order`,
//!   `InTransport`, `Delivering`, `PickupPoint`, `Delivered`, `Returning`, `BackToSender`,
//!   `Canceled`).

use chrono::{Duration, Utc};
use platform::Error;
use serde::Deserialize;
use serde_json::json;

use super::{Carriers, Created, MAX_LABEL_BYTES, ShipmentRequest, TIMEOUT, Tracked};

const NAME: &str = "PPL";
/// Batch processing is asynchronous at PPL; poll this often, 1 s apart.
const BATCH_POLLS: u32 = 10;

#[derive(Deserialize)]
struct Token {
    access_token: String,
    expires_in: Option<i64>,
}

async fn token(c: &Carriers, id: &str, secret: &str) -> Result<String, Error> {
    if let Some(t) = c.cached_ppl_token(id) {
        return Ok(t);
    }
    let res = c
        .http
        .post(format!("{}/login/getAccessToken", c.ppl_url))
        .form(&[
            ("grant_type", "client_credentials"),
            ("client_id", id),
            ("client_secret", secret),
            ("scope", "myapi2"),
        ])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    if matches!(res.status().as_u16(), 400 | 401 | 403) {
        return Err(super::rejected(NAME, "the client credentials were refused"));
    }
    if !res.status().is_success() {
        return Err(super::unavailable(NAME, format!("HTTP {}", res.status())));
    }
    let t: Token = res.json().await.map_err(|e| super::unavailable(NAME, e))?;
    let ttl = t.expires_in.unwrap_or(1800).clamp(60, 3600) - 60;
    c.store_ppl_token(
        id,
        t.access_token.clone(),
        Utc::now() + Duration::seconds(ttl),
    );
    Ok(t.access_token)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchItem {
    shipment_number: Option<String>,
    import_state: Option<String>,
    label_url: Option<String>,
    error_message: Option<String>,
}

#[derive(Deserialize)]
struct Batch {
    #[serde(default)]
    items: Vec<BatchItem>,
}

/// Only URLs on the configured API origin are followed (the label URL comes from PPL).
fn same_origin(base: &str, url: &str) -> bool {
    match (reqwest::Url::parse(base), reqwest::Url::parse(url)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

pub(crate) async fn create(
    c: &Carriers,
    id: &str,
    secret: &str,
    req: &ShipmentRequest,
) -> Result<Created, Error> {
    let bearer = token(c, id, secret).await?;
    let mut shipment = json!({
        "referenceId": req.reference,
        "productType": if req.cod_minor.is_some() { "PRID" } else { "PRIV" },
        "recipient": {
            "name": req.recipient_name,
            "street": req.street,
            "city": req.city,
            "zipCode": req.postal_code.replace(' ', ""),
            "country": req.country,
            "email": req.email,
            "phone": req.phone,
        },
        "shipmentSet": { "numberOfShipments": 1 },
    });
    if let Some(cod) = req.cod_minor {
        shipment["cashOnDelivery"] = json!({
            "codCurrency": req.currency,
            "codPrice": cod as f64 / 100.0,
            "codVarSym": req.reference,
        });
    }
    let body = json!({
        "returnChannel": { "type": "None" },
        "labelSettings": {
            "format": "Pdf",
            "dpi": 300,
            "completeLabelSettings": { "isCompleteLabelRequested": false },
        },
        "shipments": [shipment],
    });
    let res = c
        .http
        .post(format!("{}/shipment/batch", c.ppl_url))
        .bearer_auth(&bearer)
        .json(&body)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    if res.status().as_u16() == 400 {
        let detail = res.text().await.unwrap_or_default();
        return Err(super::rejected(NAME, detail));
    }
    if res.status().as_u16() != 201 {
        return Err(super::unavailable(NAME, format!("HTTP {}", res.status())));
    }
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .filter(|l| l.starts_with("/shipment/batch/") && !l.contains(".."))
        .ok_or_else(|| super::unavailable(NAME, "no batch location"))?
        .to_owned();
    let mut item = None;
    for attempt in 0..BATCH_POLLS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        let batch: Batch = c
            .http
            .get(format!("{}{location}", c.ppl_url))
            .bearer_auth(&bearer)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| super::unavailable(NAME, e))?
            .error_for_status()
            .map_err(|e| super::unavailable(NAME, e))?
            .json()
            .await
            .map_err(|e| super::unavailable(NAME, e))?;
        if let Some(i) = batch.items.into_iter().next() {
            if let Some(msg) = i.error_message.clone().filter(|m| !m.is_empty()) {
                return Err(super::rejected(NAME, msg));
            }
            if i.import_state.as_deref() == Some("Complete") && i.label_url.is_some() {
                item = Some(i);
                break;
            }
        }
    }
    let item = item.ok_or_else(|| super::unavailable(NAME, "the batch is still processing"))?;
    let number = item
        .shipment_number
        .ok_or_else(|| super::unavailable(NAME, "no shipment number"))?;
    let label_url = item.label_url.unwrap_or_default();
    if !same_origin(&c.ppl_url, &label_url) {
        return Err(super::unavailable(NAME, "label URL outside the API"));
    }
    let pdf = c
        .http
        .get(&label_url)
        .bearer_auth(&bearer)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?
        .error_for_status()
        .map_err(|e| super::unavailable(NAME, e))?
        .bytes()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    if !pdf.starts_with(b"%PDF") || pdf.len() > MAX_LABEL_BYTES {
        return Err(super::unavailable(NAME, "label is not a PDF"));
    }
    Ok(Created {
        tracking_url: format!("https://www.ppl.cz/vyhledat-zasilku?shipmentId={number}"),
        carrier_ref: number.clone(),
        tracking_number: number,
        label_pdf: pdf.to_vec(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackAndTrace {
    phase: Option<String>,
    last_event_code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShipmentInfo {
    shipment_number: String,
    track_and_trace: Option<TrackAndTrace>,
}

pub(crate) fn map_phase(phase: &str) -> Tracked {
    match phase {
        "Order" => Tracked::Announced,
        "Delivered" => Tracked::Delivered,
        "Returning" | "BackToSender" => Tracked::Returned,
        "Canceled" => Tracked::Cancelled,
        _ => Tracked::InTransit,
    }
}

pub(crate) async fn track(
    c: &Carriers,
    id: &str,
    secret: &str,
    number: &str,
) -> Result<(Tracked, String), Error> {
    let bearer = token(c, id, secret).await?;
    let list: Vec<ShipmentInfo> = c
        .http
        .get(
            reqwest::Url::parse_with_params(
                &format!("{}/shipment", c.ppl_url),
                &[("ShipmentNumbers", number)],
            )
            .map_err(|e| super::unavailable(NAME, e))?,
        )
        .bearer_auth(&bearer)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| super::unavailable(NAME, e))?
        .error_for_status()
        .map_err(|e| super::unavailable(NAME, e))?
        .json()
        .await
        .map_err(|e| super::unavailable(NAME, e))?;
    let t = list
        .into_iter()
        .find(|s| s.shipment_number == number)
        .and_then(|s| s.track_and_trace)
        .ok_or_else(|| super::unavailable(NAME, "unknown shipment"))?;
    let phase = t.phase.unwrap_or_else(|| "Order".into());
    Ok((
        map_phase(&phase),
        format!("{phase} {}", t.last_event_code.unwrap_or_default())
            .trim()
            .to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_and_origins() {
        assert_eq!(map_phase("Order"), Tracked::Announced);
        assert_eq!(map_phase("InTransport"), Tracked::InTransit);
        assert_eq!(map_phase("Delivered"), Tracked::Delivered);
        assert_eq!(map_phase("BackToSender"), Tracked::Returned);
        assert_eq!(map_phase("Canceled"), Tracked::Cancelled);
        assert!(same_origin(
            "http://mocks:4010/ppl",
            "http://mocks:4010/ppl/shipment/batch/x/label"
        ));
        assert!(!same_origin(
            "https://api.dhl.com/ecs/ppl/myapi2",
            "https://evil.example/label"
        ));
    }
}
