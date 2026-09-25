use crate::{
    controllers::pin::{mark_one, mark_pinned},
    database::{
        AppState,
        repositories::{quota_repository::QuotaGuard, song_repository::BandCopyScope},
    },
    errors::api_error::ApiError,
    export::{
        limiter::{MAX_PDF_ITEMS, ensure_pdf_fits, render_pdf},
        pdf::{
            ExportQuery, PdfExportOptions, SetlistPdfData, content_disposition,
            generate_setlist_pdf, pdf_filename,
        },
    },
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        band::{BandPermission, BandRole},
        quota::QuotaResource,
        setlist::{
            AddSongToSetlistPayload, AddedSetlistSong, BandCopyOutcome, CreateSetlistBlockPayload,
            CreateSetlistBreakPayload, CreateSetlistPayload, DuplicateSetlistPayload,
            DuplicateSetlistResponse, PublicMarker, PublicSetlist, ReorderSetlistItemsPayload,
            ReorderSetlistSongsPayload, Setlist, SetlistItem, SetlistMarker,
            UpdateSetlistBlockPayload, UpdateSetlistBreakPayload, UpdateSetlistPayload,
        },
        song::{PublicSong, SongWithArtist},
    },
    services::entitlements::{Feature, ensure_feature, has_feature, shared_content_visible},
    utils::share_token::token_fingerprint,
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

/// Most items `GET /setlists/{id}/items` returns. Every plan's setlist
/// and band repertoire limit is far below it, so no real setlist is cut;
/// it only bounds how much lyrics text one answer can carry (each song
/// may hold 50 000 characters), on top of the per-account rate limit.
pub const MAX_LISTED_ITEMS: usize = 2_000;

/// The public link of a band setlist is only shown to members who may
/// manage it (its personal owner always may): anyone else could hand it
/// out, and keep using it after leaving the band. Staff viewing as a
/// member never see it either (redacted by the middleware).
async fn withhold_share_token_from_non_managers(
    state: &AppState,
    user_id: Uuid,
    setlist: &mut Setlist,
) -> Result<(), ApiError> {
    if setlist.share_token.is_none() || setlist.band_id.is_none() {
        return Ok(());
    }
    match state.setlist_repo.can_manage(setlist.id, user_id).await {
        Ok(()) => Ok(()),
        Err(ApiError::DatabaseError(e)) => Err(ApiError::DatabaseError(e)),
        Err(_) => {
            setlist.share_token = None;
            Ok(())
        }
    }
}

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
    let (current_page, per_page) = pagination.resolve();

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
        Ok((mut setlists, total_items)) => {
            mark_pinned(&state, user_id, &mut setlists).await?;
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
        Ok(Some(mut setlist)) => {
            mark_one(&state, user_id, &mut setlist).await?;
            withhold_share_token_from_non_managers(&state, user_id, &mut setlist).await?;
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

    let quota = match payload.band_id {
        Some(band_id) => {
            // Same rule as editing a band setlist: the band's configurable
            // `manage_setlists` permission (admin/owner always pass).
            let role = state
                .band_repo
                .role_of(band_id, user_id)
                .await?
                .ok_or(ApiError::NotFound)?;

            if !state
                .band_repo
                .role_has_permission(band_id, role, BandPermission::ManageSetlists)
                .await?
            {
                return Err(ApiError::Forbidden);
            }

            state
                .quota_repo
                .band_guard(band_id, QuotaResource::BandSetlists, 1)
                .await?
        }
        None => {
            state
                .quota_repo
                .user_guard(user_id, QuotaResource::Setlists, 1)
                .await?
        }
    };

    state
        .setlist_repo
        .is_unique(payload.title.trim(), user_id, payload.band_id, None)
        .await?;

    match state.setlist_repo.create(&payload, user_id, &[quota]).await {
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
    description = "Creates an independent personal copy of a setlist the caller can view, including all of its songs, blocks, breaks and links. If the title is not provided (or already taken), a suffix such as \" (copy)\" or \" (2)\" is appended automatically.\n\nA band setlist's songs are never referenced by the copy: each becomes a song of the caller's own library (an existing song with the same title and artist is reused, otherwise the artist and song are created). Songs that would take the caller over their song or artist quota are left out and counted in `skipped_band_songs`. The answer is the new `Setlist` plus `skipped_band_songs`.\n\nDuplicating a band setlist requires the band's `export_pdf` permission (403 otherwise).",
    params(("id" = Uuid, Path, description = "The ID of the setlist to duplicate")),
    request_body = DuplicateSetlistPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Setlist duplicated successfully.", body = DuplicateSetlistResponse),
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

    // Viewing a personal setlist is enough to duplicate it — the copy
    // always lands in the caller's own personal setlists. A band setlist's
    // copy takes every chart (lyrics included) out of the band, so it needs
    // the band's `export_pdf` permission, like exporting it.
    let original = state
        .setlist_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if original.band_id.is_some() {
        state.setlist_repo.can_export_pdf(id, user_id).await?;
    }
    let quota = state
        .quota_repo
        .user_guard(user_id, QuotaResource::Setlists, 1)
        .await?;

    let limits = state.quota_repo.effective_limits(user_id).await?;

    match state
        .setlist_repo
        .duplicate(id, user_id, payload.title, limits, &[quota])
        .await
    {
        Ok((new_setlist, skipped_band_songs)) => {
            info!(%user_id, setlist_id = %id, new_setlist_id = %new_setlist.id, skipped_band_songs, "Setlist duplicated successfully");
            Ok((
                StatusCode::CREATED,
                Json(DuplicateSetlistResponse {
                    setlist: new_setlist,
                    skipped_band_songs,
                }),
            ))
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

    match state.setlist_repo.update(id, &payload, user_id).await {
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
    summary = "Move a setlist to the trash.",
    description = "The setlist disappears from lists, gigs and public links, and can be restored from the trash until it is purged. A band's repertoire can't be deleted (`REPERTOIRE_PROTECTED`).",
    params(("id" = Uuid, Path, description = "The ID of the setlist to delete")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Setlist moved to the trash"),
        (status = 409, description = "The band's repertoire can't be deleted.")
    )
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

    match state.setlist_repo.trash(id, user_id).await {
        Ok(_) => {
            info!(
                %user_id,
                setlist_id = %id,
                "Setlist moved to the trash"
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
    description = "Adds a specific song to the end of a setlist.\n\nA band setlist never links to a member's personal song: the band gets its own copy of it (and every song of a band setlist is in the band's repertoire too). When the band already has a copy of that song, it is reused, and brought up to date with the original if the band never edited it (`band_copy: updated`); a copy the band edited is left as it is (`band_copy: outdated`, see `POST /songs/{id}/sync`).",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = AddSongToSetlistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 201, description = "Song added to setlist successfully", body = AddedSetlistSong),
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
    // The song being contributed must always be one of the caller's own
    // personal songs, or a copy the setlist's band already owns.
    let source_song = state
        .song_repo
        .find_by_id(payload.song_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    // The repertoire is bounded by the band's song quota instead.
    let quota = if setlist.is_repertoire {
        None
    } else {
        Some(state.quota_repo.setlist_items_guard(setlist_id, 1).await?)
    };

    // Band setlists never link directly to a member's personal song. If the
    // song isn't already a copy owned by this band, fork it into one first:
    // this decouples the band's setlist from that member's account, so
    // editing or deleting their own original later can never take the
    // song out from under the rest of the band.
    let (song_id_to_link, band_copy) = match setlist.band_id {
        Some(band_id) => {
            let source = state
                .song_repo
                .find_with_artist_name(payload.song_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            let band_song = band_song_for(&state, band_id, &source, user_id).await?;
            (band_song.id, band_song.copy.map(|copy| (copy, source)))
        }
        None => {
            if source_song.band_id.is_some() {
                // Band copies stay inside their band.
                return Err(ApiError::NotFound);
            }
            (payload.song_id, None)
        }
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
        .add_song(setlist_id, song_id_to_link, quota.as_slice())
        .await?;
    state.setlist_repo.touch(setlist_id, user_id).await?;

    // Adding their song again is how a member brings the band up to date
    // with it: an existing copy the band never edited takes the
    // original's latest version.
    let band_copy = match band_copy {
        Some((BandCopyOutcome::Reused, source)) => {
            Some(refresh_band_copy(&state, song_id_to_link, &source, user_id).await?)
        }
        other => other.map(|(copy, _)| copy),
    };

    info!(
        %user_id,
        %setlist_id,
        song_id = %song_id_to_link,
        ?band_copy,
        "Song added to setlist successfully"
    );

    Ok((
        StatusCode::CREATED,
        Json(AddedSetlistSong {
            song_id: song_id_to_link,
            band_copy,
        }),
    ))
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/songs/{song_id}",
    tags = ["Setlists"],
    summary = "Remove a song from a setlist.",
    description = "Removes a specific song from a setlist.\n\nEvery song a band plays is in its repertoire, so taking a song out of the repertoire takes it out of the band: the band's song moves to the band's trash (it leaves every band setlist, and restoring it puts it back). This also needs the band's `manage_songs` permission.",
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
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if setlist.is_repertoire {
        if !state.setlist_repo.has_song(setlist_id, song_id).await? {
            return Err(ApiError::NotFound);
        }
        state.song_repo.can_manage(song_id, user_id).await?;
        // The song keeps its place in every setlist while it is in the
        // trash, so restoring it undoes this completely.
        state.song_repo.trash(song_id, user_id).await?;
        state.setlist_repo.touch(setlist_id, user_id).await?;
        info!(
            %user_id,
            %setlist_id,
            %song_id,
            "Song removed from the band's repertoire and moved to the trash"
        );
        return Ok(StatusCode::NO_CONTENT);
    }

    state.setlist_repo.remove_song(setlist_id, song_id).await?;
    state.setlist_repo.touch(setlist_id, user_id).await?;

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

    let (current_page, per_page) = pagination.resolve();

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
    state.setlist_repo.touch(setlist_id, user_id).await?;

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
    description = "Retrieves songs, block headers and breaks merged into a single list ordered by position — the shape the setlist builder UI renders directly. At most 2 000 items (well above every plan's limits). Each song carries its full lyrics, so the endpoint is rate limited per account: 300 calls per 5 minutes (`TOO_MANY_ATTEMPTS`, 429, `meta.retry_after_seconds`).",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Items retrieved successfully.", body = Vec<SetlistItem>),
        (status = 404, description = "Setlist not found."),
        (status = 429, description = "Too many running orders read in a short time.")
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
    // The largest ordinary answer of the API (a repertoire's every song
    // with its lyrics): bounded in size and in how often one account can
    // ask for it, so a filled-up band can't be turned into a way to keep
    // the server busy serialising and compressing tens of megabytes.
    crate::utils::rate_limit::presets::limit(
        &crate::utils::rate_limit::presets::SETLIST_ITEMS,
        user_id,
    )?;

    let mut items = state
        .setlist_repo
        .get_items_capped(id, MAX_LISTED_ITEMS)
        .await?;
    items.truncate(MAX_LISTED_ITEMS);

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
    state.setlist_repo.touch(setlist_id, user_id).await?;

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

    let quota = item_room(&state, setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .create_block(setlist_id, payload.name.trim(), quota.as_slice())
        .await?;
    state.setlist_repo.touch(setlist_id, user_id).await?;

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
        .update_block(setlist_id, marker_id, payload.name.trim())
        .await?;
    state.setlist_repo.touch(setlist_id, user_id).await?;

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

    let quota = item_room(&state, setlist_id, user_id).await?;

    let marker = state
        .setlist_repo
        .create_break(
            setlist_id,
            payload.label,
            payload.duration_minutes,
            quota.as_slice(),
        )
        .await?;
    state.setlist_repo.touch(setlist_id, user_id).await?;

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
    state.setlist_repo.touch(setlist_id, user_id).await?;

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
    state.setlist_repo.touch(setlist_id, user_id).await?;

    info!(%user_id, %setlist_id, %marker_id, "Setlist marker deleted successfully");

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/export/pdf",
    tags = ["Setlists"],
    summary = "Export a setlist to PDF.",
    description = "Generates and returns a PDF file containing the setlist's songs, blocks and breaks. Supports localization via query params. Without the plan feature `pdf_export` (the free tier) the export always carries the watermark (`watermark=false` is ignored).\n\n**Advanced options** (plan feature `advanced_pdf`, `FEATURE_NOT_IN_PLAN` otherwise): `columns=2`, the songbook (`include_lyrics=true`, with `page_break_per_song`), `watermark=false` and `margins` other than `normal`. Everything else (what to show, `compact`, `font_scale`, paper, orientation, chord mode, language, page numbers, subtitle) is available to every plan.\n\nAt most 200 items, and a songbook of at most 250 000 characters of lyrics and notes (`PDF_TOO_LARGE`, 413, `meta.reason` = `items` | `songbook`). At most 3 PDFs render at once; when busy for 10 s the answer is `SERVICE_BUSY` (503, `meta.retry_after_seconds`). Refused under impersonation (`IMPERSONATION_READ_ONLY`).",
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
        (status = 403, description = "The caller is not allowed to export this setlist to PDF."),
        (status = 404, description = "Setlist not found"),
        (status = 413, description = "The setlist is too large to export (`PDF_TOO_LARGE`)."),
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

    // Staff viewing an account read-only never walk away with its content.
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }

    crate::utils::rate_limit::presets::limit(
        &crate::utils::rate_limit::presets::PDF_EXPORT,
        user_id,
    )?;
    state.setlist_repo.can_export_pdf(id, user_id).await?;

    let setlist = state
        .setlist_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let mut options = PdfExportOptions::from(query);
    // Without a plan that includes PDF export (the free tier), setlists
    // still export, always with the watermark.
    if !has_feature(&state, user_id, Feature::PdfExport).await? {
        options.watermark = true;
    }
    if options.is_advanced() {
        ensure_feature(&state, user_id, Feature::AdvancedPdf).await?;
    }
    let items = state
        .setlist_repo
        .get_items_capped(id, MAX_PDF_ITEMS)
        .await?;
    ensure_pdf_fits(&items, options.include_lyrics)?;

    render_setlist_pdf(&state, &setlist, items, options).await
}

/// A PDF download (`Cache-Control: no-store`, RFC 5987 file name).
pub(crate) fn pdf_response(bytes: Vec<u8>, filename: &str) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    if let Ok(disposition) = HeaderValue::from_str(&content_disposition(filename)) {
        headers.insert(axum::http::header::CONTENT_DISPOSITION, disposition);
    }
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    (StatusCode::OK, headers, bytes).into_response()
}

/// Fails with `QUOTA_EXCEEDED` when the setlist is full, and otherwise
/// returns the guard that enforces the limit again in the insert's
/// transaction. A repertoire's songs are bounded by the band's song quota
/// instead, and its blocks and breaks by `MAX_REPERTOIRE_MARKERS` (checked
/// in the insert).
async fn item_room(
    state: &AppState,
    setlist_id: Uuid,
    user_id: Uuid,
) -> Result<Option<QuotaGuard>, ApiError> {
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if setlist.is_repertoire {
        return Ok(None);
    }
    Ok(Some(
        state.quota_repo.setlist_items_guard(setlist_id, 1).await?,
    ))
}

/// A band's song, resolved by [`band_song_for`].
pub(crate) struct BandSong {
    pub id: Uuid,
    /// `Created` or `Reused` when `source` was a personal song; `None` when
    /// it already was the band's.
    pub copy: Option<BandCopyOutcome>,
}

/// The band's copy of `source` for `band_id`, forking the caller's personal
/// song into the band (and resolving its artist) when needed. Band setlists
/// never link directly to a member's personal song: this decouples the
/// band from that member's account, so editing or deleting their own
/// original later can never take the song out from under the band.
pub(crate) async fn band_song_for(
    state: &AppState,
    band_id: Uuid,
    source: &SongWithArtist,
    actor_id: Uuid,
) -> Result<BandSong, ApiError> {
    if source.band_id == Some(band_id) {
        return Ok(BandSong {
            id: source.id,
            copy: None,
        });
    }
    if source.band_id.is_some() {
        // Another band's copy can't be pulled into this band.
        return Err(ApiError::NotFound);
    }
    if let Some(existing) = state.song_repo.find_band_fork(band_id, source.id).await? {
        return Ok(BandSong {
            id: existing,
            copy: Some(BandCopyOutcome::Reused),
        });
    }
    // Fails fast here; enforced again, under the band's quota lock, in the
    // copy's transaction (concurrent forks must not add up past it).
    let band_quota = state
        .quota_repo
        .band_guard(band_id, QuotaResource::BandSongs, 1)
        .await?;

    let band_artist = state
        .artist_repo
        .find_or_create_for_band(
            band_id,
            &source.artist_name,
            actor_id,
            Some(source.artist_id),
        )
        .await?;

    let forked = state
        .song_repo
        .create_band_copy(source, band_id, band_artist.id, actor_id, &[band_quota])
        .await?;

    info!(
        %actor_id, %band_id, source_song_id = %source.id, forked_song_id = %forked.id,
        "Forked a personal song into an independent band copy"
    );
    Ok(BandSong {
        id: forked.id,
        copy: Some(BandCopyOutcome::Created),
    })
}

/// Brings the band's existing copy `copy_id` up to date with `source`, the
/// actor's own original, when that is safe: the band never edited its copy
/// (nothing of theirs is lost) and the actor may edit the band's songs.
/// Anyone else's original is left alone (`Reused`).
async fn refresh_band_copy(
    state: &AppState,
    copy_id: Uuid,
    source: &SongWithArtist,
    actor_id: Uuid,
) -> Result<BandCopyOutcome, ApiError> {
    let Some(status) = state
        .song_repo
        .band_copies(actor_id, BandCopyScope::Copy(copy_id))
        .await?
        .pop()
    else {
        return Ok(BandCopyOutcome::Reused);
    };
    if !status.has_updates {
        return Ok(BandCopyOutcome::Reused);
    }
    if status.band_edited || !status.can_update {
        return Ok(BandCopyOutcome::Outdated);
    }
    sync_band_copy_from(state, status.band_id, copy_id, source, actor_id).await?;
    Ok(BandCopyOutcome::Updated)
}

/// Replaces the band copy `copy_id`'s content with `source`'s (resolving
/// the source's artist to the band's own). The caller checks permissions.
pub(crate) async fn sync_band_copy_from(
    state: &AppState,
    band_id: Uuid,
    copy_id: Uuid,
    source: &SongWithArtist,
    actor_id: Uuid,
) -> Result<(), ApiError> {
    let band_artist = state
        .artist_repo
        .find_or_create_for_band(
            band_id,
            &source.artist_name,
            actor_id,
            Some(source.artist_id),
        )
        .await?;
    state
        .song_repo
        .sync_band_copy(copy_id, source, band_artist.id, actor_id)
        .await?;
    info!(
        %actor_id, %band_id, source_song_id = %source.id, band_song_id = %copy_id,
        "Updated a band's copy of a song from its original"
    );
    Ok(())
}

/// Renders a setlist to PDF on the blocking pool, behind the global PDF
/// limit (PDF layout is CPU-bound and would otherwise stall the async
/// runtime for every other request), and wraps it in a download response.
async fn render_setlist_pdf(
    state: &AppState,
    setlist: &Setlist,
    items: Vec<SetlistItem>,
    options: PdfExportOptions,
) -> Result<axum::response::Response, ApiError> {
    let band_name = match setlist.band_id {
        Some(band_id) => state.band_repo.find_any(band_id).await?.map(|b| b.name),
        None => None,
    };

    let title = setlist.title.clone();
    let description = setlist.description.clone();
    let total_duration_secs = setlist.total_duration;

    let bytes = render_pdf(move || {
        let data = SetlistPdfData {
            title: &title,
            description: description.as_deref(),
            band_name: band_name.as_deref(),
            total_duration_secs,
            items: &items,
        };
        generate_setlist_pdf(&data, &options)
    })
    .await?;
    Ok(pdf_response(bytes, &pdf_filename(&setlist.title)))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/setlists",
    tags = ["Bands"],
    summary = "List a band's setlists.",
    description = "The caller must be a member of the band. The band's repertoire (`is_repertoire`) always comes first.",
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
    let (current_page, per_page) = pagination.resolve();

    debug!(%user_id, %band_id, current_page, per_page, "Processing request to list a band's setlists");

    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;

    let (mut setlists, total_items) = state
        .setlist_repo
        .find_all_for_band(band_id, user_id, current_page, per_page)
        .await?;
    mark_pinned(&state, user_id, &mut setlists).await?;

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

/// Filters for `GET /bands/{id}/repertoire`.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RepertoireQuery {
    /// Case-insensitive search over title and artist.
    pub q: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/repertoire",
    tags = ["Bands"],
    summary = "Songs of a band's repertoire.",
    description = "Paginated, alphabetical; for the \"add from repertoire\" picker. Any member.",
    params(("id" = Uuid, Path, description = "The ID of the band"), RepertoireQuery),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Songs.", body = PaginatedResponse<crate::models::song::SongWithArtist>),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn find_band_repertoire(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Query(query): Query<RepertoireQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (page, per_page) = crate::models::resolve_page(query.page, query.per_page, 20);
    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let search: Option<String> = query.q.map(|q| q.chars().take(100).collect());
    let (songs, total) = state
        .setlist_repo
        .get_repertoire_songs(band_id, search.as_deref(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(songs, total, page, per_page)))
}

// ---------------------------------------------------------------------
// Favorites
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/favorite",
    tags = ["Setlists"],
    summary = "Favorite a setlist.",
    description = "Purely personal to the caller — never affects anyone else's view of the setlist, and grants no permission. Idempotent. Requires the caller be able to view the setlist (its own, or any band setlist they're a member of).",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Favorited successfully."),
        (status = 404, description = "Setlist not found.")
    )
)]
pub async fn favorite_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, setlist_id = %id, "Processing request to favorite setlist");

    state.setlist_repo.exists(id, user_id).await?;
    state.setlist_repo.add_favorite(id, user_id).await?;

    info!(%user_id, setlist_id = %id, "Setlist favorited successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/favorite",
    tags = ["Setlists"],
    summary = "Un-favorite a setlist.",
    description = "Idempotent — un-favoriting a setlist that isn't favorited is not an error.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Un-favorited successfully.")
    )
)]
pub async fn unfavorite_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, setlist_id = %id, "Processing request to unfavorite setlist");

    state.setlist_repo.remove_favorite(id, user_id).await?;

    info!(%user_id, setlist_id = %id, "Setlist unfavorited successfully");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Public sharing
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/share",
    tags = ["Setlists"],
    summary = "Enable (or rotate) a public read-only share link for a setlist.",
    description = "Generates a fresh, unguessable share token, replacing any previous one — so re-sharing invalidates links that were already handed out. Requires the same permission as editing the setlist, a verified e-mail address (`EMAIL_NOT_VERIFIED`, 403) and the plan feature `public_sharing`.",
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
    crate::services::account::require_verified_email(&state, user_id).await?;
    ensure_feature(&state, user_id, Feature::PublicSharing).await?;

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
    description = "No authentication required. The token itself is the only access control — anyone who has it can view the setlist read-only: the running order (title, artist, tempo, key, duration, energy, time signature, capo, links) and the markers, never the lyrics. At most the first 2 000 songs. Answers 404 as well once the owner's plan no longer includes `public_sharing` (for band content: neither the creator's nor the band owner's plan). Rate-limited per client IP (bursts of 30, then one every 2 s).",
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
    debug!(share = %token_fingerprint(&token), "Processing request to view a public setlist");

    let setlist = shared_setlist(&state, &token).await?;

    let public = public_setlist(&state, setlist).await?;
    Ok(Json(public))
}

/// The setlist shared under `token`, or 404 when there is none or its
/// owner's plan no longer includes public sharing.
async fn shared_setlist(state: &AppState, token: &str) -> Result<Setlist, ApiError> {
    let setlist = state
        .setlist_repo
        .find_by_share_token(token)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !shared_content_visible(state, setlist.user_id, setlist.band_id).await? {
        debug!(setlist_id = %setlist.id, "Public setlist hidden: the owner's plan lost public sharing");
        return Err(ApiError::NotFound);
    }
    Ok(setlist)
}

/// Songs of a shared setlist an anonymous read returns at most (a band
/// repertoire can hold as many as the band's song quota).
pub const MAX_PUBLIC_SHARE_SONGS: i64 = 2_000;

/// The public (anonymous) view of a setlist: dedicated DTOs only (no
/// lyrics, see [`PublicSong`]), at most [`MAX_PUBLIC_SHARE_SONGS`] songs.
pub(crate) async fn public_setlist(
    state: &AppState,
    setlist: Setlist,
) -> Result<PublicSetlist, ApiError> {
    let songs = state
        .setlist_repo
        .get_positioned_songs(setlist.id, MAX_PUBLIC_SHARE_SONGS);
    let markers = state.setlist_repo.get_markers(setlist.id);
    let (songs, markers) = tokio::try_join!(songs, markers)?;

    info!(setlist_id = %setlist.id, "Public setlist retrieved successfully");

    Ok(PublicSetlist {
        title: setlist.title,
        description: setlist.description,
        total_duration: setlist.total_duration,
        links: setlist.links,
        songs: songs
            .into_iter()
            .map(|(position, song)| PublicSong::from_song(position, song))
            .collect(),
        markers: markers.into_iter().map(PublicMarker::from).collect(),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/public/setlists/{token}/export/pdf",
    tags = ["Setlists"],
    summary = "Export a publicly shared setlist to PDF.",
    description = "No authentication required — same access model as viewing it. Supports the same query params as the authenticated export endpoint, except that the songbook is never included: `include_lyrics`, `page_break_per_song` and `chords` are ignored (a share link never exposes lyrics). Other advanced options are only applied when the setlist's owner has the `advanced_pdf` plan feature (otherwise they fall back to the defaults). At most 200 items (`PDF_TOO_LARGE`, 413). 404 once the owner's plan no longer includes `public_sharing`. Rate limited per client IP (1 per second, bursts of 5) and subject to the global PDF limit (`SERVICE_BUSY`).",
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
    debug!(share = %token_fingerprint(&token), "Processing request to export a public setlist to PDF");

    let setlist = shared_setlist(&state, &token).await?;

    // A share link never yields the lyrics, whatever the query says.
    let mut options = PdfExportOptions::from(query).for_public_share();
    if options.is_advanced() && !has_feature(&state, setlist.user_id, Feature::AdvancedPdf).await? {
        options = options.to_basic();
    }
    let items = state
        .setlist_repo
        .get_items_capped(setlist.id, MAX_PDF_ITEMS)
        .await?;
    ensure_pdf_fits(&items, options.include_lyrics)?;

    render_setlist_pdf(&state, &setlist, items, options).await
}
