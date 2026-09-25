//! Email marketing Admin API (spec §8.3, §11.5, WP18): subscribers (list, unsubscribe, CSV
//! export for owner/admin with a fresh sign-in), segments (CRUD + preview) and campaigns
//! (CRUD, preview, test send, schedule, cancel, stats). Staff may run marketing; exports are
//! owner/admin only (§5.3).

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use commerce::marketing::campaigns::{self, Campaign, CampaignInput};
use commerce::marketing::segments::{self, Preview, Rules, Segment, SegmentInput};
use commerce::marketing::subscribers::{self, Subscriber, SubscriberFilter, SubscriberPage};
use commerce::tenancy::Role;
use platform::Error;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
    query_params,
};
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_subscribers))
        .routes(routes!(export_subscribers))
        .routes(routes!(unsubscribe_subscriber))
        .routes(routes!(list_segments, create_segment))
        .routes(routes!(get_segment, update_segment, delete_segment))
        .routes(routes!(preview_segment))
        .routes(routes!(list_campaigns, create_campaign))
        .routes(routes!(get_campaign, update_campaign, delete_campaign))
        .routes(routes!(preview_campaign))
        .routes(routes!(test_campaign))
        .routes(routes!(schedule_campaign))
        .routes(routes!(cancel_campaign))
}

// ---------------------------------------------------------------------------------------
// Subscribers

/// Subscribers, newest first, with filters.
#[utoipa::path(
    get,
    path = "/admin/v1/subscribers",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, SubscriberFilter),
    responses((status = 200, body = SubscriberPage))
)]
async fn list_subscribers(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<SubscriberFilter>, QueryRejection>,
) -> Result<Json<SubscriberPage>, Error> {
    let f = query_params(query)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            subscribers::list(tx, &f).await
        })
        .await?,
    ))
}

/// Every subscriber matching the filters as CSV (UTF-8). Owner or admin, signed in within the
/// last 15 minutes (A9); audited.
#[utoipa::path(
    get,
    path = "/admin/v1/subscribers/export",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, SubscriberFilter),
    responses(
        (status = 200, content_type = "text/csv", body = String),
        (status = 401, description = "reauth_required", body = platform::Problem, content_type = "application/problem+json"),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn export_subscribers(
    staff: TenantStaff,
    State(s): State<AppState>,
    query: Result<Query<SubscriberFilter>, QueryRejection>,
) -> Result<Response, Error> {
    staff.require(Role::Admin)?;
    staff.require_fresh_auth()?;
    let f = query_params(query)?;
    let actor = &staff.user.user_id;
    let csv = in_tx(&s, staff.tenant_id, async |tx| {
        subscribers::export_csv(tx, actor, &f).await
    })
    .await?;
    let mut res = csv.into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"subscribers.csv\""),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// Unsubscribes a subscriber on their request (records the withdrawal, audited).
#[utoipa::path(
    post,
    path = "/admin/v1/subscribers/{id}/unsubscribe",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Subscriber),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn unsubscribe_subscriber(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Subscriber>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            subscribers::admin_unsubscribe(tx, actor, id).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Segments

#[derive(Debug, Serialize, ToSchema)]
pub struct SegmentList {
    pub items: Vec<Segment>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/segments",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = SegmentList))
)]
async fn list_segments(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<SegmentList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| segments::list(tx).await).await?;
    Ok(Json(SegmentList { items }))
}

/// Creates a segment (rules validated against the allowlist). Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/segments",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = SegmentInput,
    responses(
        (status = 201, body = Segment),
        (status = 409, description = "name_taken", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_segment(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: SegmentInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/segments",
        &input,
        async |tx| segments::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/segments/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Segment),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_segment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Segment>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| segments::get(tx, id).await).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/admin/v1/segments/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = SegmentInput,
    responses(
        (status = 200, body = Segment),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_segment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Segment>, Error> {
    let id = path_id(id)?;
    let input: SegmentInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            segments::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes a segment (`409 segment_in_use` while campaigns use it).
#[utoipa::path(
    delete,
    path = "/admin/v1/segments/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "segment_in_use", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_segment(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        segments::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// How many subscribers the rules select right now, with a sample of 10.
#[utoipa::path(
    post,
    path = "/admin/v1/segments/preview",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = Rules,
    responses(
        (status = 200, body = Preview),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preview_segment(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<Preview>, Error> {
    let rules: Rules = parse_json(&body)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            segments::preview(tx, &rules, Utc::now()).await
        })
        .await?,
    ))
}

// ---------------------------------------------------------------------------------------
// Campaigns

#[derive(Debug, Serialize, ToSchema)]
pub struct CampaignList {
    pub items: Vec<Campaign>,
}

#[utoipa::path(
    get,
    path = "/admin/v1/campaigns",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = CampaignList))
)]
async fn list_campaigns(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<CampaignList>, Error> {
    let items = in_tx(&s, staff.tenant_id, async |tx| campaigns::list(tx).await).await?;
    Ok(Json(CampaignList { items }))
}

/// Creates a draft campaign. Honors `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/campaigns",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = CampaignInput,
    responses(
        (status = 201, body = Campaign),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: CampaignInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/campaigns",
        &input,
        async |tx| campaigns::create(tx, &actor, &input).await,
    )
    .await
}

/// A campaign with its delivery numbers.
#[utoipa::path(
    get,
    path = "/admin/v1/campaigns/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Campaign),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Campaign>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| campaigns::get(tx, id).await).await?,
    ))
}

/// Edits a draft (`409 campaign_not_draft` otherwise).
#[utoipa::path(
    put,
    path = "/admin/v1/campaigns/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = CampaignInput,
    responses(
        (status = 200, body = Campaign),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "campaign_not_draft", body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Campaign>, Error> {
    let id = path_id(id)?;
    let input: CampaignInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            campaigns::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

/// Deletes a draft.
#[utoipa::path(
    delete,
    path = "/admin/v1/campaigns/{id}",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "campaign_not_draft", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        campaigns::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewInput {
    /// Render as this subscriber gets it (language, market, personalized products).
    #[serde(default)]
    pub subscriber_id: Option<Uuid>,
    /// Without a subscriber: the language (default market).
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RenderedEmail {
    pub subject: String,
    /// Full HTML document (show it in a sandboxed iframe).
    pub html: String,
    pub text: String,
}

/// Renders the campaign for a subscriber or a language (links are not tracked).
#[utoipa::path(
    post,
    path = "/admin/v1/campaigns/{id}/preview",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = PreviewInput,
    responses(
        (status = 200, body = RenderedEmail),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn preview_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<RenderedEmail>, Error> {
    let id = path_id(id)?;
    let input: PreviewInput = parse_json(&body)?;
    let r = in_tx(&s, staff.tenant_id, async |tx| {
        campaigns::preview(
            tx,
            &s.public_urls,
            id,
            input.subscriber_id,
            input.locale.as_deref(),
            Utc::now(),
        )
        .await
    })
    .await?;
    Ok(Json(RenderedEmail {
        subject: r.subject,
        html: r.html,
        text: r.text,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestSendInput {
    /// 1-5 addresses.
    pub emails: Vec<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestSent {
    pub queued: usize,
}

/// Sends the campaign now to up to 5 addresses (subject marked `[TEST]`, not tracked, not in
/// the stats); audited.
#[utoipa::path(
    post,
    path = "/admin/v1/campaigns/{id}/test",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = TestSendInput,
    responses(
        (status = 200, body = TestSent),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn test_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<TestSent>, Error> {
    let id = path_id(id)?;
    let input: TestSendInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    let queued = in_tx(&s, staff.tenant_id, async |tx| {
        campaigns::test_send(
            tx,
            &s.public_urls,
            actor,
            id,
            &input.emails,
            input.locale.as_deref(),
            Utc::now(),
        )
        .await
    })
    .await?;
    Ok(Json(TestSent { queued }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    /// When to start; `null` or the past = now.
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
}

/// Schedules a draft (or moves a scheduled campaign). Sending runs in batches of 500 with the
/// tenant's rate limit; everyone in the segment gets it once.
#[utoipa::path(
    post,
    path = "/admin/v1/campaigns/{id}/schedule",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = ScheduleInput,
    responses(
        (status = 200, body = Campaign),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "campaign_not_schedulable", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn schedule_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Campaign>, Error> {
    let id = path_id(id)?;
    let input: ScheduleInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            campaigns::schedule(tx, actor, id, input.at, Utc::now()).await
        })
        .await?,
    ))
}

/// Stops a scheduled or sending campaign.
#[utoipa::path(
    post,
    path = "/admin/v1/campaigns/{id}/cancel",
    tag = "marketing",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Campaign),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, description = "campaign_not_active", body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn cancel_campaign(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Campaign>, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            campaigns::cancel(tx, actor, id).await
        })
        .await?,
    ))
}
