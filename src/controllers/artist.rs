use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        artist::{Artist, CreateArtistPayload, UpdateArtistPayload},
        auth::access::AccessControl,
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
    path = "/api/v1/artists",
    tags = ["Artists"],
    summary = "List all artists.",
    description = "Fetches a paginated list of artists stored in the database.",
    params(
        PaginationQuery
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Artists retrieved successfully.", body = PaginatedResponse<Artist>),
        (status = 500, description = "An error occurred while retrieving the artists.")
    )
)]
pub async fn find_all_artists(
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
        "Processing request to retrieve paginated artists"
    );

    match state
        .artist_repo
        .find_all(user_id, current_page, per_page)
        .await
    {
        Ok((artists, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(
                %user_id,
                total_items,
                total_pages,
                "Artists retrieved successfully"
            );

            Ok(Json(PaginatedResponse {
                data: artists,
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
                "Failed to retrieve artists"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/artists/{id}",
    tags = ["Artists"],
    summary = "Get a specific artist by ID.",
    description = "This endpoint retrieves an artist's details from the database using its ID.",
    params(
        ("id", description = "The unique identifier of the artist to retrieve.", example = Uuid::new_v4)
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Artist retrieved successfully.", body = Artist),
        (status = 404, description = "No artist found with the specified ID."),
        (status = 500, description = "An error occurred while retrieving the artist.")
    )
)]
pub async fn find_artist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        artist_id = %id,
        "Processing request to retrieve artist by ID"
    );

    match state.artist_repo.find_by_id(id, user_id).await {
        Ok(Some(artist)) => {
            info!(
                %user_id,
                artist_id = %id,
                "Artist retrieved successfully"
            );
            Ok(Json(artist))
        }
        Ok(None) => {
            info!(
                %user_id,
                artist_id = %id,
                "Artist not found"
            );
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!(
                %user_id,
                artist_id = %id,
                error = %e,
                "Failed to retrieve artist by ID"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/artists",
    tags = ["Artists"],
    summary = "Create a new artist.",
    description = "This endpoint creates a new artist in the database with the provided details.",
    request_body = CreateArtistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 201, description = "Artist created successfully.", body = Uuid),
        (status = 400, description = "Invalid input."),
        (status = 409, description = "Conflict: Artist with the same name already exists."),
        (status = 500, description = "An error occurred while creating the artist.")
    )
)]
pub async fn create_artist(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        artist_name = %payload.name,
        "Processing request to create a new artist"
    );

    payload.validate()?;

    state
        .artist_repo
        .is_unique(&payload.name, user_id, None)
        .await?;

    match state.artist_repo.create(&payload, user_id).await {
        Ok(new_artist) => {
            info!(
                %user_id,
                artist_id = %new_artist.id,
                "Artist created successfully"
            );

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/artists/{}", new_artist.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_artist)))
        }
        Err(e) => {
            error!(
                %user_id,
                artist_name = %payload.name,
                error = %e,
                "Failed to create artist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/artists/{id}",
    tags = ["Artists"],
    summary = "Update an existing artist.",
    description = "This endpoint updates the details of an existing artist in the database.",
    params(
        ("id" = Uuid, Path, description = "The ID of the artist to update")
    ),
    request_body = UpdateArtistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Artist updated successfully.", body = Uuid),
        (status = 400, description = "Invalid input."),
        (status = 404, description = "Artist ID not found."),
        (status = 409, description = "Conflict: Artist with the same name already exists."),
        (status = 500, description = "An error occurred while updating the artist.")
    )
)]
pub async fn update_artist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        artist_id = %id,
        "Processing request to update artist"
    );

    payload.validate()?;

    state.artist_repo.exists(id, user_id).await?;
    if let Some(name) = &payload.name {
        state.artist_repo.is_unique(name, user_id, Some(id)).await?;
    }

    match state.artist_repo.update(id, &payload).await {
        Ok(artist_id) => {
            info!(
                %user_id,
                artist_id = %artist_id,
                "Artist updated successfully"
            );
            Ok(Json(artist_id))
        }
        Err(e) => {
            error!(
                %user_id,
                artist_id = %id,
                error = %e,
                "Failed to update artist"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/artists/{id}",
    tags = ["Artists"],
    summary = "Delete an existing artist.",
    description = "This endpoint deletes a specific artist from the database using its ID.",
    params(
        ("id" = Uuid, Path, description = "The ID of the artist to delete")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
         (status = 204, description = "Artist deleted successfully"),
         (status = 404, description = "Artist ID not found"),
         (status = 500, description = "An error occurred while deleting the artist")
     )
)]
pub async fn delete_artist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    info!(
        %user_id,
        artist_id = %id,
        "Processing request to delete artist"
    );

    state.artist_repo.exists(id, user_id).await?;

    match state.artist_repo.delete(id).await {
        Ok(_) => {
            info!(
                %user_id,
                artist_id = %id,
                "Artist deleted successfully"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(
                %user_id,
                artist_id = %id,
                error = %e,
                "Failed to delete artist"
            );
            Err(e)
        }
    }
}
