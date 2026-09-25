//! Content Admin API (spec §7.5, §14, A29): pages and blog posts, menus, the legal entity,
//! legal templates and the go-live checklist.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::Utc;
use commerce::content::legal::LegalEntityView;
use commerce::content::legal::{self, GoLiveReport, InstallInput, InstallResult, LegalEntity};
use commerce::content::menus::{self, Menu, MenuInput, MenuList};
use commerce::content::{self, Page, PageFilter, PageInput, PageList};
use commerce::tenancy::Role;
use platform::Error;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::AppState;
use crate::admin::{
    IdParam, IdempotencyHeader, TenantHeader, create_idempotent, in_tx, parse_json, path_id,
    query_params,
};
use crate::admin_promotions::PageQuery;
use crate::auth::TenantStaff;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_pages, create_page))
        .routes(routes!(get_page, update_page, delete_page))
        .routes(routes!(list_menus))
        .routes(routes!(put_menu, delete_menu))
        .routes(routes!(get_legal_entity, put_legal_entity))
        .routes(routes!(install_legal_templates))
        .routes(routes!(go_live))
}

/// Pages, legal pages and blog posts, newest first.
#[utoipa::path(
    get,
    path = "/admin/v1/pages",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, PageFilter, PageQuery),
    responses((status = 200, body = PageList))
)]
async fn list_pages(
    staff: TenantStaff,
    State(s): State<AppState>,
    filter: Result<Query<PageFilter>, QueryRejection>,
    page: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<PageList>, Error> {
    let filter = query_params(filter)?;
    let q = query_params(page)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            content::list(tx, &filter, q.cursor, q.limit.unwrap_or(50)).await
        })
        .await?,
    ))
}

/// Creates a page or blog post (rich text is sanitized, links validated). Honors
/// `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/admin/v1/pages",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdempotencyHeader),
    request_body = PageInput,
    responses(
        (status = 201, body = Page),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn create_page(
    staff: TenantStaff,
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Error> {
    let input: PageInput = parse_json(&body)?;
    let actor = staff.user.user_id.clone();
    create_idempotent(
        &s,
        &staff,
        &headers,
        "POST /admin/v1/pages",
        &input,
        async |tx| content::create(tx, &actor, &input).await,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/v1/pages/{id}",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 200, body = Page),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn get_page(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Page>, Error> {
    let id = path_id(id)?;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| content::get(tx, id).await).await?,
    ))
}

/// Replaces the whole page document.
#[utoipa::path(
    put,
    path = "/admin/v1/pages/{id}",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    request_body = PageInput,
    responses(
        (status = 200, body = Page),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
        (status = 409, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn update_page(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
    body: Bytes,
) -> Result<Json<Page>, Error> {
    let id = path_id(id)?;
    let input: PageInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            content::update(tx, actor, id, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/pages/{id}",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, IdParam),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_page(
    staff: TenantStaff,
    State(s): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, Error> {
    let id = path_id(id)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        content::delete(tx, actor, id).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/admin/v1/menus",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = MenuList))
)]
async fn list_menus(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<MenuList>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| menus::list(tx).await).await?,
    ))
}

/// Creates or replaces a menu. The default theme shows `main` (header) and `footer`.
#[utoipa::path(
    put,
    path = "/admin/v1/menus/{handle}",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, ("handle" = String, Path, example = "main")),
    request_body = MenuInput,
    responses(
        (status = 200, body = Menu),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_menu(
    staff: TenantStaff,
    State(s): State<AppState>,
    handle: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<Menu>, Error> {
    let Path(handle) = handle.map_err(|_| Error::NotFound)?;
    let input: MenuInput = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            menus::put(tx, actor, &handle, &input).await
        })
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/admin/v1/menus/{handle}",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader, ("handle" = String, Path)),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn delete_menu(
    staff: TenantStaff,
    State(s): State<AppState>,
    handle: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, Error> {
    let Path(handle) = handle.map_err(|_| Error::NotFound)?;
    let actor = &staff.user.user_id;
    in_tx(&s, staff.tenant_id, async |tx| {
        menus::delete(tx, actor, &handle).await
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The seller's legal identity (fills the legal templates; checked before go-live).
#[utoipa::path(
    get,
    path = "/admin/v1/legal-entity",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = LegalEntityView))
)]
async fn get_legal_entity(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<LegalEntityView>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| legal::entity(tx).await).await?,
    ))
}

/// Saves the legal entity (owner/admin). Fields may be incomplete; go-live lists the gaps.
#[utoipa::path(
    put,
    path = "/admin/v1/legal-entity",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = LegalEntity,
    responses(
        (status = 200, body = LegalEntityView),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn put_legal_entity(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<LegalEntityView>, Error> {
    staff.require(Role::Admin)?;
    let input: LegalEntity = parse_json(&body)?;
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            legal::put_entity(tx, actor, &input).await
        })
        .await?,
    ))
}

/// Installs the platform legal templates (cs/sk/en) as draft legal pages filled from the legal
/// entity and tax profile (owner/admin). Existing legal pages are kept. The templates are not
/// legal advice: review them with a lawyer before publishing.
#[utoipa::path(
    post,
    path = "/admin/v1/legal/templates/install",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    request_body = InstallInput,
    responses(
        (status = 200, body = InstallResult),
        (status = 403, body = platform::Problem, content_type = "application/problem+json"),
        (status = 422, body = platform::Problem, content_type = "application/problem+json"),
    )
)]
async fn install_legal_templates(
    staff: TenantStaff,
    State(s): State<AppState>,
    body: Bytes,
) -> Result<Json<InstallResult>, Error> {
    staff.require(Role::Admin)?;
    let input: InstallInput = if body.is_empty() {
        InstallInput::default()
    } else {
        parse_json(&body)?
    };
    let actor = &staff.user.user_id;
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            legal::install(tx, actor, &input).await
        })
        .await?,
    ))
}

/// Go-live checklist (A29): legal entity fields, tax profile, published legal pages in every
/// market's default locale, GPSR manufacturer on active products.
#[utoipa::path(
    get,
    path = "/admin/v1/go-live",
    tag = "content",
    security(("staff_jwt" = [])),
    params(TenantHeader),
    responses((status = 200, body = GoLiveReport))
)]
async fn go_live(
    staff: TenantStaff,
    State(s): State<AppState>,
) -> Result<Json<GoLiveReport>, Error> {
    Ok(Json(
        in_tx(&s, staff.tenant_id, async |tx| {
            legal::go_live(tx, Utc::now()).await
        })
        .await?,
    ))
}
