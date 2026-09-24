//! Tours: a named run of gigs between two dates.

use crate::{
    controllers::pin::{mark_one, mark_pinned},
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse,
        auth::access::AccessControl,
        band::{BandPermission, BandRole},
        gig::GigStatus,
        quota::QuotaResource,
        resolve_page,
        tour::{
            CreateTourPayload, Tour, TourDetail, TourListQuery, TourStats, UpdateTourPayload,
            check_dates,
        },
    },
    services::entitlements::{Feature, ensure_feature},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use tracing::info;
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(
    get,
    path = "/api/v1/tours",
    tags = ["Tours"],
    summary = "List the caller's personal tours.",
    description = "`status`: `upcoming` (default; not ended yet, soonest first), `past` (most recent first) or `all`.",
    params(TourListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Tours.", body = PaginatedResponse<Tour>))
)]
pub async fn find_all_tours(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<TourListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let (mut tours, total) = state
        .tour_repo
        .find_all(user_id, query.status.unwrap_or_default(), page, per_page)
        .await?;
    mark_pinned(&state, user_id, &mut tours).await?;
    Ok(Json(PaginatedResponse::new(tours, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/tours",
    tags = ["Tours"],
    summary = "List a band's tours.",
    description = "Any member. Same `status` filter as `GET /tours`.",
    params(("id" = Uuid, Path, description = "The ID of the band"), TourListQuery),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Tours.", body = PaginatedResponse<Tour>),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn find_band_tours(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Query(query): Query<TourListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let (mut tours, total) = state
        .tour_repo
        .find_all_for_band(band_id, query.status.unwrap_or_default(), page, per_page)
        .await?;
    mark_pinned(&state, user_id, &mut tours).await?;
    Ok(Json(PaginatedResponse::new(tours, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/tours",
    tags = ["Tours"],
    summary = "Create a tour.",
    description = "Plan feature `tours`. Personal tours count against the `tours` quota, band tours against `band_tours` and need the band's `manage_setlists` permission. `end_date` can't be before `start_date`.",
    request_body = CreateTourPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Tour created.", body = Tour),
        (status = 400, description = "Invalid input."),
        (status = 403, description = "Plan feature, quota or band permission.")
    )
)]
pub async fn create_tour(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateTourPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    ensure_feature(&state, user_id, Feature::Tours).await?;

    let quota = match payload.band_id {
        Some(band_id) => {
            state
                .band_repo
                .require_permission(band_id, user_id, BandPermission::ManageSetlists)
                .await?;
            state
                .quota_repo
                .band_guard(band_id, QuotaResource::BandTours, 1)
                .await?
        }
        None => {
            state
                .quota_repo
                .user_guard(user_id, QuotaResource::Tours, 1)
                .await?
        }
    };

    let tour = state.tour_repo.create(&payload, user_id, &[quota]).await?;
    info!(%user_id, tour_id = %tour.id, "Tour created");

    let mut headers = HeaderMap::new();
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/tours/{}", tour.id)) {
        headers.insert(LOCATION, location);
    }
    Ok((StatusCode::CREATED, headers, Json(tour)))
}

#[utoipa::path(
    get,
    path = "/api/v1/tours/{id}",
    tags = ["Tours"],
    summary = "A tour with its gigs and totals.",
    description = "The tour's fields plus `gigs` (soonest first, each with its setlist `{id, title, song_count, total_duration}` or `null`) and `stats` `{total_gigs, confirmed, completed, cancelled, total_setlist_duration}` (seconds).",
    params(("id" = Uuid, Path, description = "The tour ID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Tour.", body = TourDetail),
        (status = 404, description = "Tour not found.")
    )
)]
pub async fn find_tour_by_id(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let mut tour = state
        .tour_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    mark_one(&state, user_id, &mut tour).await?;
    let gigs = state.tour_repo.gigs(id).await?;

    let mut stats = TourStats {
        total_gigs: gigs.len() as i64,
        ..TourStats::default()
    };
    for gig in &gigs {
        match gig.status {
            GigStatus::Confirmed => stats.confirmed += 1,
            GigStatus::Completed => stats.completed += 1,
            GigStatus::Cancelled => stats.cancelled += 1,
        }
        if let Some(setlist) = &gig.setlist {
            stats.total_setlist_duration += i64::from(setlist.total_duration);
        }
    }

    Ok(Json(TourDetail { tour, gigs, stats }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/tours/{id}",
    tags = ["Tours"],
    summary = "Update a tour.",
    description = "Absent fields stay; `description: null` clears it. Returns the updated tour.",
    params(("id" = Uuid, Path, description = "The tour ID")),
    request_body = UpdateTourPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Updated tour.", body = Tour),
        (status = 403, description = "No permission."),
        (status = 404, description = "Tour not found.")
    )
)]
pub async fn update_tour(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateTourPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    state.tour_repo.can_manage(id, user_id).await?;

    let current = state
        .tour_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_dates(
        payload.start_date.unwrap_or(current.start_date),
        payload.end_date.unwrap_or(current.end_date),
    )?;

    state.tour_repo.update(id, &payload, user_id).await?;
    let mut tour = state
        .tour_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    mark_one(&state, user_id, &mut tour).await?;
    Ok(Json(tour))
}

#[utoipa::path(
    delete,
    path = "/api/v1/tours/{id}",
    tags = ["Tours"],
    summary = "Move a tour to the trash.",
    description = "Its gigs stay (without a tour until the tour is restored).",
    params(("id" = Uuid, Path, description = "The tour ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Tour moved to the trash."))
)]
pub async fn delete_tour(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state.tour_repo.can_manage(id, user_id).await?;
    state.tour_repo.trash(id, user_id).await?;
    info!(%user_id, tour_id = %id, "Tour moved to the trash");
    Ok(StatusCode::NO_CONTENT)
}
