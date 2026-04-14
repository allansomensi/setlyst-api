use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        artist::{Artist, ArtistPublic, CreateArtistPayload, UpdateArtistPayload},
        auth::access::AccessControl,
    },
    validations::{existence::artist_exists, uniqueness::is_artist_unique},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

/// Retrieves a list of all artists.
///
/// This endpoint fetches artists stored in the database according to pagination parameters.
/// If there are no artists, returns an empty array.
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
        (status = 200, description = "Artists retrieved successfully.", body = PaginatedResponse<ArtistPublic>),
        (status = 500, description = "An error occurred while retrieving the artists.")
    )
)]
pub async fn find_all_artists(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve all artists.");

    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    match Artist::find_all(&state, access.user().id, current_page, per_page).await {
        Ok((artists, total_items)) => {
            info!("Artists listed successfully. Total: {total_items}");

            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

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
            error!("Error retrieving all artists: {e}");
            Err(e)
        }
    }
}

/// Retrieves a specific artist by its ID.
///
/// This endpoint searches for an artist with the specified ID.
/// If the artist is found, it returns the artist details.
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
        (status = 200, description = "Artist retrieved successfully.", body = ArtistPublic),
        (status = 404, description = "No artist found with the specified ID."),
        (status = 500, description = "An error occurred while retrieving the artist.")
    )
)]
pub async fn find_artist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve artist with id: {id}");

    match Artist::find_by_id(&state, id, access.user().id).await {
        Ok(Some(artist)) => {
            info!("Artist found: {id}");
            Ok(Json(artist))
        }
        Ok(None) => {
            error!("No artist found with id: {id}");
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!("Error retrieving artist with id {id}: {e}");
            Err(e)
        }
    }
}

/// Create a new artist.
///
/// This endpoint creates a new artist by providing its details.
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
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<CreateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(
        "Received request to create artist with name: {}",
        payload.name
    );

    payload.validate()?;
    is_artist_unique(&state, &payload.name, access.user().id).await?;

    match Artist::create(&state, &payload, access.user().id).await {
        Ok(new_artist) => {
            info!("Artist created! ID: {}", &new_artist.id);

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/artists/{}", new_artist.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_artist.id)))
        }
        Err(e) => {
            error!("Error creating artist with name {}: {e}", payload.name);
            Err(e)
        }
    }
}

/// Updates an existing artist.
///
/// This endpoint updates the details of an existing artist.
/// It accepts the artist ID in the path and the new details in the body.
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
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateArtistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to update artist with ID: {id}");

    payload.validate()?;
    artist_exists(&state, id, access.user().id).await?;
    is_artist_unique(&state, &payload.name, access.user().id).await?;

    match Artist::update(&state, id, &payload).await {
        Ok(artist_id) => {
            info!("Artist updated! ID: {artist_id}");
            Ok(Json(artist_id))
        }
        Err(e) => {
            error!("Error updating artist with ID {id}: {e}");
            Err(e)
        }
    }
}

/// Deletes an existing artist.
///
/// This endpoint allows artists to delete a specific artist by its ID.
/// It checks if the artist exists before attempting to delete it.
/// If the artist is successfully deleted, a 204 status code is returned.
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
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to delete artist with ID: {id}");

    artist_exists(&state, id, access.user().id).await?;

    match Artist::delete(&state, id).await {
        Ok(_) => {
            info!("Artist deleted! ID: {id}");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!("Error deleting artist with ID {id}: {e}");
            Err(e)
        }
    }
}
