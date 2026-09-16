use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        band::BandRole,
        gig::{CreateGigPayload, Gig, PublicGig, UpdateGigPayload},
        setlist::PublicSetlist,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

/// Ensures a `setlist_id` a caller wants to attach to a gig actually
/// belongs to the same scope as the gig: the same band, or the caller's
/// own personal setlist when the gig is personal. Reused by both create
/// and update so the invariant can never drift out of a linked setlist
/// belonging to a different band than its gig.
async fn validate_setlist_scope(
    state: &AppState,
    user_id: Uuid,
    setlist_id: Uuid,
    gig_band_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if setlist.band_id != gig_band_id {
        error!(%setlist_id, ?gig_band_id, "Setlist does not belong to the same band as the gig.");
        return Err(ApiError::BadRequest(
            "The linked setlist must belong to the same band as the gig (or be a personal setlist for a personal gig).".to_string(),
        ));
    }

    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/gigs",
    tags = ["Gigs"],
    summary = "List the caller's personal gigs.",
    description = "Fetches a paginated list of the caller's own personal (non-band) gigs, soonest first.",
    params(PaginationQuery),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Gigs retrieved successfully.", body = PaginatedResponse<Gig>),
        (status = 500, description = "An error occurred while retrieving the gigs.")
    )
)]
pub async fn find_all_gigs(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    debug!(%user_id, current_page, per_page, "Processing request to retrieve paginated gigs");

    match state
        .gig_repo
        .find_all(user_id, current_page, per_page)
        .await
    {
        Ok((gigs, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(%user_id, total_items, "Gigs retrieved successfully");

            Ok(Json(PaginatedResponse {
                data: gigs,
                meta: PaginationMeta {
                    total_items,
                    current_page,
                    per_page,
                    total_pages,
                },
            }))
        }
        Err(e) => {
            error!(%user_id, error = %e, "Failed to retrieve gigs");
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/gigs/{id}",
    tags = ["Gigs"],
    summary = "Get a specific gig by ID.",
    description = "This endpoint retrieves a gig's details from the database using its ID.",
    params(("id", description = "The unique identifier of the gig to retrieve.", example = Uuid::new_v4)),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Gig retrieved successfully.", body = Gig),
        (status = 404, description = "No gig found with the specified ID.")
    )
)]
pub async fn find_gig_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, gig_id = %id, "Processing request to retrieve gig by ID");

    match state.gig_repo.find_by_id(id, user_id).await {
        Ok(Some(gig)) => {
            info!(%user_id, gig_id = %id, "Gig retrieved successfully");
            Ok(Json(gig))
        }
        Ok(None) => {
            info!(%user_id, gig_id = %id, "Gig not found");
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!(%user_id, gig_id = %id, error = %e, "Failed to retrieve gig by ID");
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/gigs",
    tags = ["Gigs"],
    summary = "Create a new gig.",
    description = "This endpoint creates a new gig in the database with the provided details.",
    request_body = CreateGigPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Gig created successfully.", body = Gig),
        (status = 400, description = "Invalid input."),
        (status = 403, description = "The caller does not have permission to create gigs for this band.")
    )
)]
pub async fn create_gig(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateGigPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, venue = %payload.venue, band_id = ?payload.band_id, "Processing request to create a new gig");

    payload.validate()?;

    if let Some(band_id) = payload.band_id {
        let band = state
            .band_repo
            .find_by_id(band_id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        let can_create = band.my_role.satisfies(BandRole::Moderator)
            || (band.my_role.satisfies(BandRole::Member) && band.members_can_manage_setlists);

        if !can_create {
            return Err(ApiError::Forbidden);
        }
    }

    if let Some(setlist_id) = payload.setlist_id {
        validate_setlist_scope(&state, user_id, setlist_id, payload.band_id).await?;
    }

    match state.gig_repo.create(&payload, user_id).await {
        Ok(new_gig) => {
            info!(%user_id, gig_id = %new_gig.id, "Gig created successfully");

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/gigs/{}", new_gig.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_gig)))
        }
        Err(e) => {
            error!(%user_id, venue = %payload.venue, error = %e, "Failed to create gig");
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/gigs/{id}",
    tags = ["Gigs"],
    summary = "Update an existing gig.",
    description = "This endpoint updates the details of an existing gig in the database.",
    params(("id" = Uuid, Path, description = "The ID of the gig to update")),
    request_body = UpdateGigPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Gig updated successfully.", body = Uuid),
        (status = 404, description = "Gig ID not found.")
    )
)]
pub async fn update_gig(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateGigPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, gig_id = %id, "Processing request to update gig");

    payload.validate()?;

    state.gig_repo.can_manage(id, user_id).await?;

    if let Some(setlist_id) = payload.setlist_id {
        let gig = state
            .gig_repo
            .find_by_id(id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        validate_setlist_scope(&state, user_id, setlist_id, gig.band_id).await?;
    }

    match state.gig_repo.update(id, &payload).await {
        Ok(gig_id) => {
            info!(%user_id, gig_id = %gig_id, "Gig updated successfully");
            Ok(Json(gig_id))
        }
        Err(e) => {
            error!(%user_id, gig_id = %id, error = %e, "Failed to update gig");
            Err(e)
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/gigs/{id}",
    tags = ["Gigs"],
    summary = "Delete an existing gig.",
    description = "This endpoint deletes a specific gig from the database using its ID.",
    params(("id" = Uuid, Path, description = "The ID of the gig to delete")),
    security((), ("jwt_token" = [])),
    responses((status = 204, description = "Gig deleted successfully"))
)]
pub async fn delete_gig(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, gig_id = %id, "Processing request to delete gig");

    state.gig_repo.can_manage(id, user_id).await?;

    match state.gig_repo.delete(id).await {
        Ok(_) => {
            info!(%user_id, gig_id = %id, "Gig deleted successfully");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(%user_id, gig_id = %id, error = %e, "Failed to delete gig");
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/gigs",
    tags = ["Bands"],
    summary = "List a band's gigs.",
    description = "The caller must be a member of the band.",
    params(("id" = Uuid, Path, description = "The ID of the band"), PaginationQuery),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Gigs retrieved successfully.", body = PaginatedResponse<Gig>),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn find_band_gigs(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    debug!(%user_id, %band_id, current_page, per_page, "Processing request to list a band's gigs");

    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;

    let (gigs, total_items) = state
        .gig_repo
        .find_all_for_band(band_id, current_page, per_page)
        .await?;

    let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

    info!(%user_id, %band_id, total_items, "Band gigs retrieved successfully");

    Ok(Json(PaginatedResponse {
        data: gigs,
        meta: PaginationMeta {
            total_items,
            current_page,
            per_page,
            total_pages,
        },
    }))
}

// ---------------------------------------------------------------------
// Public sharing
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/gigs/{id}/share",
    tags = ["Gigs"],
    summary = "Enable (or rotate) a public read-only share link for a gig.",
    description = "Generates a fresh, unguessable share token, replacing any previous one — so re-sharing invalidates links that were already handed out. Requires the same permission as editing the gig.",
    params(("id" = Uuid, Path, description = "The ID of the gig")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Sharing enabled successfully.", body = Gig),
        (status = 403, description = "The caller does not have permission to manage this gig."),
        (status = 404, description = "Gig not found.")
    )
)]
pub async fn enable_gig_sharing(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, gig_id = %id, "Processing request to enable public sharing for gig");

    state.gig_repo.can_manage(id, user_id).await?;

    let gig = state.gig_repo.enable_sharing(id).await?;

    info!(%user_id, gig_id = %id, "Public sharing enabled successfully");
    Ok(Json(gig))
}

#[utoipa::path(
    delete,
    path = "/api/v1/gigs/{id}/share",
    tags = ["Gigs"],
    summary = "Disable public sharing for a gig.",
    description = "Immediately invalidates the existing share link, if any. Requires the same permission as editing the gig.",
    params(("id" = Uuid, Path, description = "The ID of the gig")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Sharing disabled successfully"),
        (status = 403, description = "The caller does not have permission to manage this gig."),
        (status = 404, description = "Gig not found.")
    )
)]
pub async fn disable_gig_sharing(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, gig_id = %id, "Processing request to disable public sharing for gig");

    state.gig_repo.can_manage(id, user_id).await?;
    state.gig_repo.disable_sharing(id).await?;

    info!(%user_id, gig_id = %id, "Public sharing disabled successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/public/gigs/{token}",
    tags = ["Gigs"],
    summary = "View a publicly shared gig.",
    description = "No authentication required. The token itself is the only access control — anyone who has it can view the gig, and its linked setlist, read-only.",
    params(("token" = String, Path, description = "The gig's public share token")),
    responses(
        (status = 200, description = "Gig retrieved successfully.", body = PublicGig),
        (status = 404, description = "No gig is shared under this token.")
    )
)]
pub async fn get_public_gig(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(share_token = %token, "Processing request to view a public gig");

    let gig = state
        .gig_repo
        .find_by_share_token(&token)
        .await?
        .ok_or(ApiError::NotFound)?;

    let setlist = match gig.setlist_id {
        Some(setlist_id) => {
            let setlist = state
                .setlist_repo
                .find_by_id(setlist_id, gig.user_id)
                .await?;
            match setlist {
                Some(setlist) => {
                    let songs = state.setlist_repo.get_songs(setlist.id, 1, 200);
                    let markers = state.setlist_repo.get_markers(setlist.id);
                    let ((songs, _), markers) = tokio::try_join!(songs, markers)?;
                    Some(PublicSetlist {
                        title: setlist.title,
                        description: setlist.description,
                        total_duration: setlist.total_duration,
                        songs,
                        markers,
                    })
                }
                None => None,
            }
        }
        None => None,
    };

    info!(gig_id = %gig.id, "Public gig retrieved successfully");

    Ok(Json(PublicGig {
        venue: gig.venue,
        location: gig.location,
        scheduled_at: gig.scheduled_at,
        status: gig.status,
        setlist,
    }))
}
