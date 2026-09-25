//! Consent (A20): `POST /_p/consent` on the shop and checkout origins lands here through the
//! edge. The anonymous subject id travels in `X-Consent-Subject` (the edge's consent cookie);
//! a new one is minted on the first choice and returned in the same header, together with
//! `X-Consent-Summary` for the script-readable cookie (see `docs/decisions/consent-contract.md`).
//! On the checkout origin a signed-in customer's choice is also recorded for the customer.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use commerce::consent::{self, ConsentChoice, ConsentState, Purposes, Subject};
use commerce::customers;
use platform::Error;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::customer::{CONSENT_SUBJECT_HEADER, SESSION_HEADER, header_str, ip_hash, no_store};
use super::{Shopper, StorefrontHeaders, with_ctx};
use crate::AppState;
use crate::admin::parse_json;

pub const SUMMARY_HEADER: &str = "x-consent-summary";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_consent, post_consent))
}

/// The consent subject and session (documentation only).
#[derive(IntoParams)]
#[into_params(parameter_in = Header)]
#[allow(dead_code)]
pub(crate) struct ConsentHeaders {
    /// Anonymous subject id from the consent cookie (32 hex characters).
    #[param(rename = "X-Consent-Subject")]
    x_consent_subject: Option<String>,
    /// Checkout origin only: the signed-in customer's session.
    #[param(rename = "X-Customer-Session")]
    x_customer_session: Option<String>,
    /// The client's IP (stored as a salted hash with the record).
    #[param(rename = "X-Client-Ip")]
    x_client_ip: Option<String>,
    /// The browser's user agent (ad platforms that require it, WP20).
    #[param(rename = "X-Client-User-Agent")]
    x_client_user_agent: Option<String>,
}

fn anon(headers: &HeaderMap) -> Option<String> {
    header_str(headers, CONSENT_SUBJECT_HEADER)
        .filter(|s| consent::well_formed_anon(s))
        .map(str::to_owned)
}

/// The current choice: cookie purposes from the anonymous subject (this browser), email
/// purposes from the signed-in customer (checkout origin). `text_version: null` means no choice
/// yet.
#[utoipa::path(
    get,
    path = "/storefront/v1/consent",
    tag = "storefront",
    params(StorefrontHeaders, ConsentHeaders),
    responses((status = 200, body = ConsentState))
)]
async fn get_consent(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let anon = anon(&headers);
    let session = header_str(&headers, SESSION_HEADER);
    let state = with_ctx(&s, &shopper, async |tx, _| {
        let customer = match session {
            Some(t) => customers::authenticate(tx, t).await?,
            None => None,
        };
        match (customer, anon) {
            // Cookie purposes are per browser (the anonymous subject); email purposes belong
            // to the account.
            (Some(c), Some(a)) => {
                let mut device = consent::state(tx, &Subject::Anon(a)).await?;
                let account = consent::state(tx, &Subject::Customer(c.customer_id)).await?;
                device.purposes.email_marketing = account.purposes.email_marketing;
                device.purposes.review_invites = account.purposes.review_invites;
                if device.text_version.is_none() {
                    device.text_version = account.text_version;
                }
                Ok(device)
            }
            (Some(c), None) => consent::state(tx, &Subject::Customer(c.customer_id)).await,
            (None, Some(a)) => consent::state(tx, &Subject::Anon(a)).await,
            (None, None) => Ok(ConsentState {
                purposes: Purposes::default(),
                text_version: None,
            }),
        }
    })
    .await?;
    Ok(no_store(Json(state).into_response()))
}

/// Records a choice for the anonymous subject (minted on the first choice) and, when signed in
/// on the checkout origin, for the customer. Answers the anonymous subject's new state.
#[utoipa::path(
    post,
    path = "/storefront/v1/consent",
    tag = "storefront",
    params(StorefrontHeaders, ConsentHeaders),
    request_body = ConsentChoice,
    responses(
        (status = 200, body = ConsentState, headers(
            ("X-Consent-Subject" = String, description = "The anonymous subject id (for the edge's cookie)"),
            ("X-Consent-Summary" = String, description = "Granted purposes, comma-separated, for the script-readable `consent` cookie"),
        )),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn post_consent(
    shopper: Shopper,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let choice: ConsentChoice = parse_json(&body)?;
    let subject = anon(&headers).unwrap_or_else(consent::new_anon_id);
    let session = header_str(&headers, SESSION_HEADER);
    let state = with_ctx(&s, &shopper, async |tx, _| {
        let ip = ip_hash(tx, &headers).await?;
        let anon = Subject::Anon(subject.clone());
        consent::record(tx, &anon, &choice, ip.as_deref()).await?;
        if let Some(t) = session
            && let Some(c) = customers::authenticate(tx, t).await?
        {
            consent::record(
                tx,
                &Subject::Customer(c.customer_id),
                &choice,
                ip.as_deref(),
            )
            .await?;
        }
        consent::state(tx, &anon).await
    })
    .await?;
    let mut res = Json(&state).into_response();
    let h = res.headers_mut();
    h.insert(
        CONSENT_SUBJECT_HEADER,
        HeaderValue::from_str(&subject).map_err(|e| Error::Internal(e.to_string()))?,
    );
    if let Some(summary) = consent::summary(&state) {
        h.insert(
            SUMMARY_HEADER,
            HeaderValue::from_str(&summary).map_err(|e| Error::Internal(e.to_string()))?,
        );
    }
    Ok(no_store(res))
}
