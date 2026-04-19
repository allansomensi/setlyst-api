use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        setlist::{
            AddSongToSetlistPayload, CreateSetlistPayload, ReorderSetlistSongsPayload, Setlist,
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
        "Processing request to create a new setlist"
    );

    payload.validate()?;

    state
        .setlist_repo
        .is_unique(&payload.title, user_id, None)
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

    state.setlist_repo.exists(id, user_id).await?;

    if let Some(title) = &payload.title {
        state
            .setlist_repo
            .is_unique(title, user_id, Some(id))
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

    state.setlist_repo.exists(id, user_id).await?;

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

    state.setlist_repo.exists(setlist_id, user_id).await?;
    state.song_repo.exists(payload.song_id, user_id).await?;

    state
        .setlist_repo
        .add_song(setlist_id, payload.song_id, payload.position)
        .await?;

    info!(
        %user_id,
        %setlist_id,
        song_id = %payload.song_id,
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

    state.setlist_repo.exists(setlist_id, user_id).await?;
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
        (status = 200, description = "Songs retrieved successfully", body = PaginatedResponse<crate::models::song::Song>),
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

    state.setlist_repo.exists(setlist_id, user_id).await?;

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
