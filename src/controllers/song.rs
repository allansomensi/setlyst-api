use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        song::{CreateSongPayload, Song, UpdateSongPayload},
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
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "List all songs.",
    description = "Fetches a paginated list of songs stored in the database.",
    params(PaginationQuery),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Songs retrieved successfully.", body = PaginatedResponse<Song>),
        (status = 500, description = "An error occurred while retrieving the songs.")
    )
)]
pub async fn find_all_songs(
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
        "Processing request to retrieve paginated songs"
    );

    match state
        .song_repo
        .find_all(user_id, current_page, per_page)
        .await
    {
        Ok((songs, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(
                %user_id,
                total_items,
                total_pages,
                "Songs retrieved successfully"
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
                error = %e,
                "Failed to retrieve songs"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Get a specific song by ID.",
    description = "This endpoint retrieves a song's details from the database using its ID.",
    params(("id", description = "The unique identifier of the song to retrieve.", example = Uuid::new_v4)),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Song retrieved successfully.", body = Song),
        (status = 404, description = "No song found with the specified ID.")
    )
)]
pub async fn find_song_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        song_id = %id,
        "Processing request to retrieve song by ID"
    );

    match state.song_repo.find_by_id(id, user_id).await {
        Ok(Some(song)) => {
            info!(
                %user_id,
                song_id = %id,
                "Song retrieved successfully"
            );
            Ok(Json(song))
        }
        Ok(None) => {
            info!(
                %user_id,
                song_id = %id,
                "Song not found"
            );
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!(
                %user_id,
                song_id = %id,
                error = %e,
                "Failed to retrieve song by ID"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/songs",
    tags = ["Songs"],
    summary = "Create a new song.",
    description = "This endpoint creates a new song in the database with the provided details.",
    request_body = CreateSongPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Song created successfully.", body = Song),
        (status = 400, description = "Invalid input."),
        (status = 409, description = "Conflict: Song already exists for this artist.")
    )
)]
pub async fn create_song(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        song_title = %payload.title,
        artist_id = %payload.artist_id,
        "Processing request to create a new song"
    );

    payload.validate()?;

    state.artist_repo.exists(payload.artist_id, user_id).await?;
    state
        .song_repo
        .is_unique(&payload.title, payload.artist_id, user_id, None)
        .await?;

    match state.song_repo.create(&payload, user_id).await {
        Ok(new_song) => {
            info!(
                %user_id,
                song_id = %new_song.id,
                "Song created successfully"
            );

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/songs/{}", new_song.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_song)))
        }
        Err(e) => {
            error!(
                %user_id,
                song_title = %payload.title,
                error = %e,
                "Failed to create song"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Update an existing song.",
    description = "This endpoint updates the details of an existing song in the database.",
    params(("id" = Uuid, Path, description = "The ID of the song to update")),
    request_body = UpdateSongPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Song updated successfully.", body = Uuid),
        (status = 404, description = "Song ID not found.")
    )
)]
pub async fn update_song(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        song_id = %id,
        "Processing request to update song"
    );

    payload.validate()?;

    let existing_song = state
        .song_repo
        .find_by_id(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if let Some(artist_id) = payload.artist_id {
        state.artist_repo.exists(artist_id, user_id).await?;
    }

    if payload.title.is_some() || payload.artist_id.is_some() {
        let title_to_check = payload.title.as_deref().unwrap_or(&existing_song.title);
        let artist_to_check = payload.artist_id.unwrap_or(existing_song.artist_id);

        state
            .song_repo
            .is_unique(title_to_check, artist_to_check, user_id, Some(id))
            .await?;
    }

    match state.song_repo.update(id, &payload).await {
        Ok(song_id) => {
            info!(
                %user_id,
                song_id = %song_id,
                "Song updated successfully"
            );
            Ok(Json(song_id))
        }
        Err(e) => {
            error!(
                %user_id,
                song_id = %id,
                error = %e,
                "Failed to update song"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/songs/{id}",
    tags = ["Songs"],
    summary = "Delete an existing song.",
    description = "This endpoint deletes a specific song from the database using its ID.",
    params(("id" = Uuid, Path, description = "The ID of the song to delete")),
    security((), ("jwt_token" = [])),
    responses((status = 204, description = "Song deleted successfully"))
)]
pub async fn delete_song(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        song_id = %id,
        "Processing request to delete song"
    );

    state.song_repo.exists(id, user_id).await?;

    match state.song_repo.delete(id).await {
        Ok(_) => {
            info!(
                %user_id,
                song_id = %id,
                "Song deleted successfully"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(
                %user_id,
                song_id = %id,
                error = %e,
                "Failed to delete song"
            );
            Err(e)
        }
    }
}
