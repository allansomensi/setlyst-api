use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        setlist::{CreateSetlistPayload, Setlist, SetlistPublic, UpdateSetlistPayload},
    },
    validations::existence::{setlist_exists, song_exists},
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

/// Retrieves a list of all setlists.
///
/// This endpoint fetches setlists stored in the database according to pagination parameters.
/// If there are no setlists, returns an empty array.
#[utoipa::path(
    get,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    summary = "List all setlists.",
    description = "Fetches a paginated list of setlists stored in the database.",
    params(
        PaginationQuery
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Setlists retrieved successfully.", body = PaginatedResponse<SetlistPublic>),
        (status = 500, description = "An error occurred while retrieving the setlists.")
    )
)]
pub async fn find_all_setlists(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve all setlists.");

    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    match Setlist::find_all(&state, access.user().id, current_page, per_page).await {
        Ok((setlists, total_items)) => {
            info!("Setlists listed successfully. Total: {total_items}");

            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

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
            error!("Error retrieving all setlists: {e}");
            Err(e)
        }
    }
}

/// Retrieves a specific setlist by its ID.
///
/// This endpoint searches for a setlist with the specified ID.
/// If the setlist is found, it returns the setlist details along with its songs.
#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Get a specific setlist by ID.",
    description = "This endpoint retrieves a setlist's details from the database using its ID.",
    params(
        ("id", description = "The unique identifier of the setlist to retrieve.", example = Uuid::new_v4)
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Setlist retrieved successfully.", body = SetlistPublic),
        (status = 404, description = "No setlist found with the specified ID."),
        (status = 500, description = "An error occurred while retrieving the setlist.")
    )
)]
pub async fn find_setlist_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve setlist with id: {id}");

    match Setlist::find_by_id(&state, id, access.user().id).await {
        Ok(Some(setlist)) => {
            info!("Setlist found: {id}");
            Ok(Json(setlist))
        }
        Ok(None) => {
            error!("No setlist found with id: {id}");
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!("Error retrieving setlist with id {id}: {e}");
            Err(e)
        }
    }
}

/// Create a new setlist.
///
/// This endpoint creates a new setlist by providing its details and associated songs.
#[utoipa::path(
    post,
    path = "/api/v1/setlists",
    tags = ["Setlists"],
    summary = "Create a new setlist.",
    description = "This endpoint creates a new setlist in the database with the provided details.",
    request_body = CreateSetlistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 201, description = "Setlist created successfully.", body = Uuid),
        (status = 400, description = "Invalid input."),
        (status = 404, description = "Song not found."),
        (status = 500, description = "An error occurred while creating the setlist.")
    )
)]
pub async fn create_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(
        "Received request to create setlist with title: {}",
        payload.title
    );

    payload.validate()?;
    let user_id = access.user().id;

    for song_id in &payload.song_ids {
        song_exists(&state, *song_id, user_id).await?;
    }

    match Setlist::create(&state, &payload, user_id).await {
        Ok(new_setlist) => {
            info!("Setlist created! ID: {}", &new_setlist.id);

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/setlists/{}", new_setlist.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_setlist.id)))
        }
        Err(e) => {
            error!("Error creating setlist with title {}: {e}", payload.title);
            Err(e)
        }
    }
}

/// Updates an existing setlist.
///
/// This endpoint updates the details of an existing setlist.
/// It accepts the setlist ID in the path and the new details in the body.
#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Update an existing setlist.",
    description = "This endpoint updates the details of an existing setlist in the database.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist to update")
    ),
    request_body = UpdateSetlistPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Setlist updated successfully.", body = Uuid),
        (status = 400, description = "Invalid input."),
        (status = 404, description = "Setlist or Song ID not found."),
        (status = 500, description = "An error occurred while updating the setlist.")
    )
)]
pub async fn update_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to update setlist with ID: {id}");

    payload.validate()?;
    let user_id = access.user().id;

    setlist_exists(&state, id, user_id).await?;

    if let Some(song_ids) = &payload.song_ids {
        for song_id in song_ids {
            song_exists(&state, *song_id, user_id).await?;
        }
    }

    match Setlist::update(&state, id, &payload).await {
        Ok(setlist_id) => {
            info!("Setlist updated! ID: {setlist_id}");
            Ok(Json(setlist_id))
        }
        Err(e) => {
            error!("Error updating setlist with ID {id}: {e}");
            Err(e)
        }
    }
}

/// Deletes an existing setlist.
///
/// This endpoint allows users to delete a specific setlist by its ID.
/// It checks if the setlist exists before attempting to delete it.
/// If the setlist is successfully deleted, a 204 status code is returned.
#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}",
    tags = ["Setlists"],
    summary = "Delete an existing setlist.",
    description = "This endpoint deletes a specific setlist from the database using its ID.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist to delete")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "Setlist deleted successfully"),
        (status = 404, description = "Setlist ID not found"),
        (status = 500, description = "An error occurred while deleting the setlist")
    )
)]
pub async fn delete_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to delete setlist with ID: {id}");

    setlist_exists(&state, id, access.user().id).await?;

    match Setlist::delete(&state, id).await {
        Ok(_) => {
            info!("Setlist deleted! ID: {id}");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!("Error deleting setlist with ID {id}: {e}");
            Err(e)
        }
    }
}
