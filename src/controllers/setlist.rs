use crate::{
    database::AppState,
    errors::api_error::ApiError,
    export::pdf::{ExportQuery, PdfExportOptions, generate_setlist_pdf},
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        band::BandRole,
        setlist::{
            AddSongToSetlistPayload, CreateSetlistBlockPayload, CreateSetlistBreakPayload,
            CreateSetlistPayload, DuplicateSetlistPayload, PublicSetlist,
            ReorderSetlistItemsPayload, ReorderSetlistSongsPayload, Setlist, SetlistItem,
            SetlistMarker, UpdateSetlistBlockPayload, UpdateSetlistBreakPayload,
            UpdateSetlistPayload,
        },
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

#[utoipa::path(
    get,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    summary = "List all setlists.",
    description = "Fetches a paginated list of setlists stored in the database.",
    params(PaginationQuery),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlists retrieved successfully.", body = PaginatedResponse<Setlist>),
        (status = 500, description = "An error occurred while retrieving the setlists.")
    )
)]
pub async fn find_all_setlists(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    debug!(
        %user_id,
        current_page,
        per_page,
        "Processing request to retrieve paginated setlists"
    );

    match state
        .setlist_repo
        .find_all(user_id, current_page, per_page)
        .await
    {
        Ok((setlists, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(
                %user_id,
                total_items,
                total_pages,
                "Setlists retrieved successfully"
            );

            Ok(Json(PaginatedResponse {
                data: setlists,
                meta: PaginationMeta {
                    total_items,
                    current_page,
                    per_page,
                    total_pages,
                },
            }))
        }
        Err(e) => {
            error!(
                %user_id,
                error = %e,
                "Failed to retrieve setlists"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Get a specific setlist by ID.",
    description = "This endpoint retrieves a setlist's details from the database using its ID.",
    params(("id", description = "The unique identifier of the setlist to retrieve.", example = Uuid::new_v4)),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlist retrieved successfully.", body = Setlist),
        (status = 404, description = "No setlist found with the specified ID.")
    )
)]
pub async fn find_setlist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        setlist_id = %id,
        "Processing request to retrieve setlist by ID"
    );

    match state.setlist_repo.find_by_id(id, access.user_id()).await {
        Ok(Some(setlist)) => {
            info!(
                %user_id,
                setlist_id = %id,
                "Setlist retrieved successfully"
            );
            Ok(Json(setlist))
        }
        Ok(None) => {
            info!(
                %user_id,
                setlist_id = %id,
                "Setlist not found"
            );
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!(
                %user_id,
                setlist_id = %id,
                error = %e,
                "Failed to retrieve setlist by ID"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    summary = "Create a new setlist.",
    description = "This endpoint creates a new setlist in the database with the provided details.",
    request_body = CreateSetlistPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Setlist created successfully.", body = Setlist),
        (status = 400, description = "Invalid input."),
        (status = 409, description = "Conflict: Setlist already exists.")
    )
)]
pub async fn create_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        setlist_title = %payload.title,
        band_id = ?payload.band_id,
        "Processing request to create a new setlist"
    );

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

    state
        .setlist_repo
        .is_unique(&payload.title, user_id, payload.band_id, None)
        .await?;

    match state.setlist_repo.create(&payload, user_id).await {
        Ok(new_setlist) => {
            info!(
                %user_id,
                setlist_id = %new_setlist.id,
                "Setlist created successfully"
            );

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/setlists/{}", new_setlist.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_setlist)))
        }
        Err(e) => {
            error!(
                %user_id,
                setlist_title = %payload.title,
                error = %e,
                "Failed to create setlist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/duplicate",
    tags = ["Setlists"],
    summary = "Duplicate a setlist.",
    description = "Creates an independent personal copy of a setlist the caller can view, including all of its songs, blocks and breaks. If the title is not provided (or already taken), a suffix such as \" (copy)\" or \" (2)\" is appended automatically.",
    params(("id" = Uuid, Path, description = "The ID of the setlist to duplicate")),
    request_body = DuplicateSetlistPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Setlist duplicated successfully.", body = Setlist),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn duplicate_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<DuplicateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, setlist_id = %id, "Processing request to duplicate setlist");

    payload.validate()?;

    // Viewing the setlist is enough to duplicate it — the copy always
    // lands in the caller's own personal setlists, so no write access to
    // the original (e.g. a band setlist) is required.
    state.setlist_repo.exists(id, user_id).await?;

    match state
        .setlist_repo
        .duplicate(id, user_id, payload.title)
        .await
    {
        Ok(new_setlist) => {
            info!(%user_id, setlist_id = %id, new_setlist_id = %new_setlist.id, "Setlist duplicated successfully");
            Ok((StatusCode::CREATED, Json(new_setlist)))
        }
        Err(e) => {
            error!(%user_id, setlist_id = %id, error = %e, "Failed to duplicate setlist");
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Update an existing setlist.",
    description = "This endpoint updates the details of an existing setlist in the database.",
    params(("id" = Uuid, Path, description = "The ID of the setlist to update")),
    request_body = UpdateSetlistPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlist updated successfully.", body = Uuid),
        (status = 404, description = "Setlist ID not found.")
    )
)]
pub async fn update_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        setlist_id = %id,
        "Processing request to update setlist"
    );

    payload.validate()?;

    state.setlist_repo.can_manage(id, user_id).await?;

    if let Some(title) = &payload.title {
        let setlist = state
            .setlist_repo
            .find_by_id(id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        state
            .setlist_repo
            .is_unique(title, user_id, setlist.band_id, Some(id))
            .await?;
    }

    match state.setlist_repo.update(id, &payload).await {
        Ok(setlist_id) => {
            info!(
                %user_id,
                setlist_id = %setlist_id,
                "Setlist updated successfully"
            );
            Ok(Json(setlist_id))
        }
        Err(e) => {
            error!(
                %user_id,
                setlist_id = %id,
                error = %e,
                "Failed to update setlist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Delete an existing setlist.",
    description = "This endpoint deletes a specific setlist from the database using its ID.",
    params(("id" = Uuid, Path, description = "The ID of the setlist to delete")),
    security((), ("jwt_token" = [])),
    responses((status = 204, description = "Setlist deleted successfully"))
)]
pub async fn delete_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(
        %user_id,
        setlist_id = %id,
        "Processing request to delete setlist"
    );

    state.setlist_repo.can_manage(id, user_id).await?;

    match state.setlist_repo.delete(id).await {
        Ok(_) => {
            info!(
                %user_id,
                setlist_id = %id,
                "Setlist deleted successfully"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(
                %user_id,
                setlist_id = %id,
                error = %e,
                "Failed to delete setlist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/songs",
    tags = ["Setlists"],
    summary = "Add a song to a setlist.",
    description = "Adds a specific song to a setlist at a given position.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = AddSongToSetlistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 201, description = "Song added to setlist successfully", body = String),
        (status = 404, description = "Setlist or song not found.")
    )
)]
pub async fn add_song_to_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<AddSongToSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        %setlist_id,
        song_id = %payload.song_id,
        "Processing request to add song to setlist"
    );

    state.setlist_repo.can_manage(setlist_id, user_id).await?;
    // The song being contributed must always be the caller's own — this
    // holds regardless of whether it ends up personal or forked into a band.
    state.song_repo.exists(payload.song_id, user_id).await?;

    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // Band setlists never link directly to a member's personal song. If the
    // song isn't already a copy owned by this band, fork it into one first:
    // this decouples the band's setlist from that member's account, so
    // editing or deleting their own original later can never take the
    // song out from under the rest of the band.
    let song_id_to_link = match setlist.band_id {
        Some(band_id) => {
            let source = state
                .song_repo
                .find_with_artist_name(payload.song_id)
                .await?
                .ok_or(ApiError::NotFound)?;

            if source.band_id == Some(band_id) {
                payload.song_id
            } else {
                let band_artist = state
                    .artist_repo
                    .find_or_create_for_band(band_id, &source.artist_name, user_id)
                    .await?;

                let forked = state
                    .song_repo
                    .create_band_copy(&source, band_id, band_artist.id, user_id)
                    .await?;

                info!(
                    %user_id, %band_id, source_song_id = %payload.song_id, forked_song_id = %forked.id,
                    "Forked a personal song into an independent band copy"
                );

                forked.id
            }
        }
        None => payload.song_id,
    };

    // A setlist never plays the same song twice — reject outright rather
    // than silently repositioning it, so the person knows their click
    // didn't do what they expected.
    if state
        .setlist_repo
        .has_song(setlist_id, song_id_to_link)
        .await?
    {
        error!(%user_id, %setlist_id, song_id = %song_id_to_link, "Song is already in this setlist.");
        return Err(ApiError::AlreadyExists);
    }

    state
        .setlist_repo
        .add_song(setlist_id, song_id_to_link, payload.position)
        .await?;

    info!(
        %user_id,
        %setlist_id,
        song_id = %song_id_to_link,
        "Song added to setlist successfully"
    );

    Ok((
        StatusCode::CREATED,
        Json("Song added to setlist successfully"),
    ))
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/songs/{song_id}",
    tags = ["Setlists"],
    summary = "Remove a song from a setlist.",
    description = "Removes a specific song from a setlist.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("song_id" = Uuid, Path, description = "The ID of the song to remove")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "Song removed successfully"),
        (status = 404, description = "Setlist or song not found.")
    )
)]
pub async fn remove_song_from_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, song_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        %setlist_id,
        %song_id,
        "Processing request to remove song from setlist"
    );

    state.setlist_repo.can_manage(setlist_id, user_id).await?;
    state.setlist_repo.remove_song(setlist_id, song_id).await?;

    info!(
        %user_id,
        %setlist_id,
        %song_id,
        "Song removed from setlist successfully"
    );

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/songs",
    tags = ["Setlists"],
    summary = "Get all songs in a setlist.",
    description = "Retrieves a paginated list of all songs associated with a specific setlist, ordered by their position.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        PaginationQuery
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Songs retrieved successfully", body = PaginatedResponse<crate::models::song::SongWithArtist>),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn get_setlist_songs(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    info!(
        %user_id,
        setlist_id = %id,
        current_page,
        per_page,
        "Processing request to retrieve songs for setlist"
    );

    state.setlist_repo.exists(id, user_id).await?;

    match state
        .setlist_repo
        .get_songs(id, current_page, per_page)
        .await
    {
        Ok((songs, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(
                %user_id,
                setlist_id = %id,
                total_items,
                "Songs for setlist retrieved successfully"
            );

            Ok(Json(PaginatedResponse {
                data: songs,
                meta: PaginationMeta {
                    total_items,
                    current_page,
                    per_page,
                    total_pages,
                },
            }))
        }
        Err(e) => {
            error!(
                %user_id,
                setlist_id = %id,
                error = %e,
                "Failed to retrieve songs for setlist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}/songs/reorder",
    tags = ["Setlists"],
    summary = "Reorder songs in a setlist.",
    description = "Updates the positions of all songs in a setlist based on the provided ordered list of IDs.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist")
    ),
    request_body = ReorderSetlistSongsPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Setlist reordered successfully"),
        (status = 400, description = "Invalid input."),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn reorder_setlist_songs(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<ReorderSetlistSongsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        %setlist_id,
        "Processing request to reorder songs in setlist"
    );

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;

    state
        .setlist_repo
        .reorder_songs(setlist_id, &payload.song_ids)
        .await?;

    info!(
        %user_id,
        %setlist_id,
        "Songs in setlist reordered successfully"
    );

    Ok((StatusCode::OK, Json("Setlist reordered successfully")))
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/items",
    tags = ["Setlists"],
    summary = "Get the full running order of a setlist.",
    description = "Retrieves songs, block headers and breaks merged into a single list ordered by position — the shape the setlist builder UI renders directly.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Items retrieved successfully.", body = Vec<SetlistItem>),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn get_setlist_items(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, setlist_id = %id, "Processing request to retrieve setlist items");

    state.setlist_repo.exists(id, user_id).await?;

    let items = state.setlist_repo.get_items(id).await?;

    Ok(Json(items))
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}/items/reorder",
    tags = ["Setlists"],
    summary = "Reorder songs, blocks and breaks in a setlist.",
    description = "Updates the shared position of every song and marker (block/break) to match the given order.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = ReorderSetlistItemsPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlist reordered successfully."),
        (status = 400, description = "Invalid input."),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn reorder_setlist_items(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<ReorderSetlistItemsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, "Processing request to reorder setlist items");

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;
    state
        .setlist_repo
        .reorder_items(setlist_id, &payload.items)
        .await?;

    info!(%user_id, %setlist_id, "Setlist items reordered successfully");

    Ok((StatusCode::OK, Json("Setlist reordered successfully")))
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/blocks",
    tags = ["Setlists"],
    summary = "Add a named block to a setlist.",
    description = "Creates a block/section header (e.g. \"Bloco Baladas\") appended at the end of the setlist's running order. Reorder it into place afterwards via /items/reorder.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = CreateSetlistBlockPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Block created successfully.", body = SetlistMarker),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn create_setlist_block(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<CreateSetlistBlockPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, "Processing request to create setlist block");

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .create_block(setlist_id, &payload.name)
        .await?;

    info!(%user_id, %setlist_id, marker_id = %marker.id, "Setlist block created successfully");

    Ok((StatusCode::CREATED, Json(marker)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}/blocks/{marker_id}",
    tags = ["Setlists"],
    summary = "Rename a setlist block.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("marker_id" = Uuid, Path, description = "The ID of the block")
    ),
    request_body = UpdateSetlistBlockPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Block updated successfully.", body = SetlistMarker),
        (status = 404, description = "Setlist or block not found.")
    )
)]
pub async fn update_setlist_block(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, marker_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateSetlistBlockPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, %marker_id, "Processing request to update setlist block");

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .update_block(setlist_id, marker_id, &payload.name)
        .await?;

    info!(%user_id, %setlist_id, %marker_id, "Setlist block updated successfully");

    Ok(Json(marker))
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/breaks",
    tags = ["Setlists"],
    summary = "Add a break/pause to a setlist.",
    description = "Creates a visual break (optional label and duration in minutes) appended at the end of the setlist's running order. Reorder it into place afterwards via /items/reorder.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = CreateSetlistBreakPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Break created successfully.", body = SetlistMarker),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn create_setlist_break(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<CreateSetlistBreakPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, "Processing request to create setlist break");

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .create_break(setlist_id, payload.label, payload.duration_minutes)
        .await?;

    info!(%user_id, %setlist_id, marker_id = %marker.id, "Setlist break created successfully");

    Ok((StatusCode::CREATED, Json(marker)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}/breaks/{marker_id}",
    tags = ["Setlists"],
    summary = "Update a setlist break.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("marker_id" = Uuid, Path, description = "The ID of the break")
    ),
    request_body = UpdateSetlistBreakPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Break updated successfully.", body = SetlistMarker),
        (status = 404, description = "Setlist or break not found.")
    )
)]
pub async fn update_setlist_break(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, marker_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateSetlistBreakPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, %marker_id, "Processing request to update setlist break");

    payload.validate()?;

    state.setlist_repo.can_manage(setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .update_break(
            setlist_id,
            marker_id,
            payload.label,
            payload.duration_minutes,
        )
        .await?;

    info!(%user_id, %setlist_id, %marker_id, "Setlist break updated successfully");

    Ok(Json(marker))
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/markers/{marker_id}",
    tags = ["Setlists"],
    summary = "Delete a setlist block or break.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("marker_id" = Uuid, Path, description = "The ID of the block or break")
    ),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Marker deleted successfully"),
        (status = 404, description = "Setlist or marker not found.")
    )
)]
pub async fn delete_setlist_marker(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, marker_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, %setlist_id, %marker_id, "Processing request to delete setlist marker");

    state.setlist_repo.can_manage(setlist_id, user_id).await?;
    state
        .setlist_repo
        .delete_marker(setlist_id, marker_id)
        .await?;

    info!(%user_id, %setlist_id, %marker_id, "Setlist marker deleted successfully");

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/export/pdf",
    tags = ["Setlists"],
    summary = "Export a setlist to PDF.",
    description = "Generates and returns a PDF file containing the setlist's songs. Supports localization via query params.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist to export"),
        ExportQuery
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "PDF exported successfully", content_type = "application/pdf"),
        (status = 404, description = "Setlist not found"),
        (status = 500, description = "An error occurred while exporting the setlist")
    )
)]
pub async fn export_setlist_pdf(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Query(query): Query<ExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, setlist_id = %id, "Processing request to export setlist to PDF");

    let setlist = state
        .setlist_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let (songs, _) = state.setlist_repo.get_songs(id, 1, 100).await?;

    let options = PdfExportOptions::from(query);

    match generate_setlist_pdf(&setlist.title, setlist.total_duration, &songs, &options) {
        Ok(pdf_bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/pdf"),
            );

            let filename = format!(
                "setlist-{}.pdf",
                setlist.title.replace(" ", "-").to_lowercase()
            );

            if let Ok(disposition) =
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            {
                headers.insert(axum::http::header::CONTENT_DISPOSITION, disposition);
            }

            info!(%user_id, setlist_id = %id, "Setlist PDF exported successfully");

            Ok((StatusCode::OK, headers, pdf_bytes).into_response())
        }
        Err(e) => {
            error!(%user_id, setlist_id = %id, error = ?e, "Failed to generate PDF");
            Ok((StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate PDF").into_response())
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/setlists",
    tags = ["Bands"],
    summary = "List a band's setlists.",
    description = "The caller must be a member of the band.",
    params(("id" = Uuid, Path, description = "The ID of the band"), PaginationQuery),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Setlists retrieved successfully.", body = PaginatedResponse<Setlist>),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn find_band_setlists(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    debug!(%user_id, %band_id, current_page, per_page, "Processing request to list a band's setlists");

    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;

    let (setlists, total_items) = state
        .setlist_repo
        .find_all_for_band(band_id, current_page, per_page)
        .await?;

    let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

    info!(%user_id, %band_id, total_items, "Band setlists retrieved successfully");

    Ok(Json(PaginatedResponse {
        data: setlists,
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
    path = "/api/v1/setlists/{id}/share",
    tags = ["Setlists"],
    summary = "Enable (or rotate) a public read-only share link for a setlist.",
    description = "Generates a fresh, unguessable share token, replacing any previous one — so re-sharing invalidates links that were already handed out. Requires the same permission as editing the setlist.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Sharing enabled successfully.", body = Setlist),
        (status = 403, description = "The caller does not have permission to manage this setlist."),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn enable_setlist_sharing(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, setlist_id = %id, "Processing request to enable public sharing for setlist");

    state.setlist_repo.can_manage(id, user_id).await?;

    let setlist = state.setlist_repo.enable_sharing(id).await?;

    info!(%user_id, setlist_id = %id, "Public sharing enabled successfully");
    Ok(Json(setlist))
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/share",
    tags = ["Setlists"],
    summary = "Disable public sharing for a setlist.",
    description = "Immediately invalidates the existing share link, if any. Requires the same permission as editing the setlist.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Sharing disabled successfully"),
        (status = 403, description = "The caller does not have permission to manage this setlist."),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn disable_setlist_sharing(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, setlist_id = %id, "Processing request to disable public sharing for setlist");

    state.setlist_repo.can_manage(id, user_id).await?;
    state.setlist_repo.disable_sharing(id).await?;

    info!(%user_id, setlist_id = %id, "Public sharing disabled successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/public/setlists/{token}",
    tags = ["Setlists"],
    summary = "View a publicly shared setlist.",
    description = "No authentication required. The token itself is the only access control — anyone who has it can view the setlist read-only.",
    params(("token" = String, Path, description = "The setlist's public share token")),
    responses(
        (status = 200, description = "Setlist retrieved successfully.", body = PublicSetlist),
        (status = 404, description = "No setlist is shared under this token.")
    )
)]
pub async fn get_public_setlist(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(share_token = %token, "Processing request to view a public setlist");

    let setlist = state
        .setlist_repo
        .find_by_share_token(&token)
        .await?
        .ok_or(ApiError::NotFound)?;

    let songs = state.setlist_repo.get_songs(setlist.id, 1, 200);
    let markers = state.setlist_repo.get_markers(setlist.id);
    let ((songs, _), markers) = tokio::try_join!(songs, markers)?;

    info!(setlist_id = %setlist.id, "Public setlist retrieved successfully");

    Ok(Json(PublicSetlist {
        title: setlist.title,
        description: setlist.description,
        total_duration: setlist.total_duration,
        songs,
        markers,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/public/setlists/{token}/export/pdf",
    tags = ["Setlists"],
    summary = "Export a publicly shared setlist to PDF.",
    description = "No authentication required — same access model as viewing it. Supports the same localization query params as the authenticated export endpoint.",
    params(
        ("token" = String, Path, description = "The setlist's public share token"),
        ExportQuery
    ),
    responses(
        (status = 200, description = "PDF exported successfully", content_type = "application/pdf"),
        (status = 404, description = "No setlist is shared under this token.")
    )
)]
pub async fn export_public_setlist_pdf(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(query): Query<ExportQuery>,
) -> Result<axum::response::Response, ApiError> {
    debug!(share_token = %token, "Processing request to export a public setlist to PDF");

    let setlist = state
        .setlist_repo
        .find_by_share_token(&token)
        .await?
        .ok_or(ApiError::NotFound)?;

    let (songs, _) = state.setlist_repo.get_songs(setlist.id, 1, 200).await?;

    let options = PdfExportOptions::from(query);

    match generate_setlist_pdf(&setlist.title, setlist.total_duration, &songs, &options) {
        Ok(pdf_bytes) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/pdf"),
            );

            let filename = format!(
                "setlist-{}.pdf",
                setlist.title.replace(" ", "-").to_lowercase()
            );

            if let Ok(disposition) =
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            {
                headers.insert(axum::http::header::CONTENT_DISPOSITION, disposition);
            }

            info!(setlist_id = %setlist.id, "Public setlist PDF exported successfully");

            Ok((StatusCode::OK, headers, pdf_bytes).into_response())
        }
        Err(e) => {
            error!(setlist_id = %setlist.id, error = ?e, "Failed to generate PDF for public setlist");
            Ok((StatusCode::INTERNAL_SERVER_ERROR, "Failed to generate PDF").into_response())
        }
    }
}
