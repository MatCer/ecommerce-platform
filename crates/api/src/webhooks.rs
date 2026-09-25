//! Payment provider webhooks (spec §10.4, A10, A11). WP10 has the fake gateway's
//! (`PAYMENTS_FAKE=1`); Stripe's arrives with WP11 on the same pattern: verify the signature
//! over the raw body before parsing, check the event against the attempt (tenant, amount,
//! currency), then apply the outcome idempotently.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use commerce::payments::{self, FakeEvent};
use platform::Error;
use platform::db::tenant_tx;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::admin::parse_json;

pub const FAKE_SIGNATURE_HEADER: &str = "x-fake-signature";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(fake_webhook))
}

/// Verifies and applies a fake provider event. The tenant comes from the signed body; the
/// attempt must belong to it and match the amount and currency.
pub(crate) async fn fake_event(s: &AppState, signature: &str, raw: &[u8]) -> Result<(), Error> {
    let gateway = s.checkout.payments.fake.as_ref().ok_or(Error::NotFound)?;
    gateway.verify(signature, raw, Utc::now())?;
    let event: FakeEvent = parse_json(raw)?;
    let mut tx = tenant_tx(&s.db, event.tenant_id).await?;
    let attempt = payments::attempt(&mut tx, event.attempt_id).await?;
    if attempt.method != payments::MethodKind::Fake
        || attempt.amount_minor != event.amount_minor
        || attempt.currency != event.currency
    {
        return Err(Error::Validation {
            code: "event_mismatch",
            detail: "the event does not match the payment attempt".into(),
        });
    }
    payments::apply_outcome(&mut tx, attempt.id, event.outcome, "fake_gateway").await?;
    tx.commit().await?;
    Ok(())
}

/// Fake provider events (`PAYMENTS_FAKE=1` only, else `404`). Signed like outgoing webhooks
/// (§8.5): `X-Fake-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`,
/// at most 5 minutes old. Repeated events are no-ops.
#[utoipa::path(
    post,
    path = "/webhooks/fake",
    tag = "webhooks",
    params(("X-Fake-Signature" = String, Header)),
    request_body = FakeEvent,
    responses(
        (status = 204, description = "Applied (or already applied)"),
        (status = 401, description = "invalid_signature", body = platform::Problem, content_type = "application/problem+json"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "attempt_finished", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, description = "event_mismatch", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn fake_webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Error> {
    let signature = headers
        .get(FAKE_SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or(Error::Unauthorized {
            code: "invalid_signature",
        })?;
    fake_event(&s, signature, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}
