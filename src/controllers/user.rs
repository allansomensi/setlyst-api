use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        user::{CreateUserPayload, Role, UpdateUserPayload, User, UserPublic},
    },
    validations::{existence::user_exists, uniqueness::is_user_unique},
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

/// Retrieves a list of all users.
///
/// This endpoint fetches a paginated list of users stored in the database.
/// If there are no users, returns an empty array.
#[utoipa::path(
    get,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "List all users.",
    description = "Fetches a paginated list of users stored in the database.",
    params(
        PaginationQuery
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Users retrieved successfully.", body = PaginatedResponse<UserPublic>),
        (status = 500, description = "An error occurred while retrieving the users.")
    )
)]
pub async fn find_all_users(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve all users.");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    match User::find_all(&state, current_page, per_page).await {
        Ok((users, total_items)) => {
            info!("Users listed successfully. Total: {total_items}");

            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            Ok(Json(PaginatedResponse {
                data: users,
                meta: PaginationMeta {
                    total_items,
                    current_page,
                    per_page,
                    total_pages,
                },
            }))
        }
        Err(e) => {
            error!("Error retrieving all users: {e}");
            Err(e)
        }
    }
}

/// Retrieves a specific user by its ID.
///
/// This endpoint searches for a user with the specified ID.
/// If the user is found, it returns the user details.
#[utoipa::path(
    get,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Get a specific user by ID.",
    description = "This endpoint retrieves a user's details from the database using its ID. Returns the user if found, or a 404 status if not found.",
    params(
        ("id", description = "The unique identifier of the user to retrieve.", example = Uuid::new_v4)
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "User retrieved successfully.", body = UserPublic),
        (status = 404, description = "No user found with the specified ID."),
        (status = 500, description = "An error occurred while retrieving the user.")
    )
)]
pub async fn find_user_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve user with id: {id}");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match User::find_by_id(&state, id).await {
        Ok(Some(user)) => {
            info!("User found: {id}");
            Ok(Json(user))
        }
        Ok(None) => {
            error!("No user found with id: {id}");
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!("Error retrieving user with id {id}: {e}");
            Err(e)
        }
    }
}

/// Create a new user.
///
/// This endpoint creates a new user by providing its details.
/// Validates the user's name for length and emptiness, checks for duplicates,
/// and inserts the new user into the database if all validations pass.
#[utoipa::path(
    post,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "Create a new user.",
    description = "This endpoint creates a new user in the database with the provided details.",
    request_body = CreateUserPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 201, description = "User created successfully.", body = Uuid),
        (status = 400, description = "Invalid input, including empty name or name too short/long."),
        (status = 409, description = "Conflict: User with the same name already exists."),
        (status = 500, description = "An error occurred while creating the user.")
    )
)]
pub async fn create_user(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(
        "Received request to create user with username: {}",
        payload.username
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    payload.validate()?;
    is_user_unique(&state, &payload.username).await?;

    match User::create(&state, &payload).await {
        Ok(new_user) => {
            info!("User created! ID: {}", &new_user.id);

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/users/{}", new_user.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((StatusCode::CREATED, headers, Json(new_user.id)))
        }
        Err(e) => {
            error!(
                "Error creating user with username {}: {e}",
                payload.username
            );
            Err(e)
        }
    }
}

/// Updates an existing user.
///
/// This endpoint updates the details of an existing user.
/// It accepts the user ID in the path and the new details in the body.
#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Update an existing user.",
    description = "This endpoint updates the details of an existing user in the database.",
    params(
        ("id" = Uuid, Path, description = "The ID of the user to update")
    ),
    request_body = UpdateUserPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "User updated successfully.", body = Uuid),
        (status = 400, description = "Invalid input, including empty name or name too short/long."),
        (status = 404, description = "User ID not found."),
        (status = 409, description = "Conflict: User with the same name already exists."),
        (status = 500, description = "An error occurred while updating the user.")
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to update user with ID: {id}");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    payload.validate()?;
    user_exists(&state, id).await?;

    match User::update(&state, id, &payload).await {
        Ok(user_id) => {
            info!("User updated! ID: {user_id}");
            Ok(Json(user_id))
        }
        Err(e) => {
            error!("Error updating user with ID {id}: {e}");
            Err(e)
        }
    }
}

/// Deletes an existing user.
///
/// This endpoint allows users to delete a specific user by its ID.
/// It checks if the user exists before attempting to delete it.
/// If the user is successfully deleted, a 204 status code is returned.
#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Delete an existing user.",
    description = "This endpoint deletes a specific user from the database using its ID.",
    params(
        ("id" = Uuid, Path, description = "The ID of the user to delete")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "User deleted successfully"),
        (status = 404, description = "User ID not found"),
        (status = 500, description = "An error occurred while deleting the user")
    )
)]
pub async fn delete_user(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to delete user with ID: {id}");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    user_exists(&state, id).await?;

    match User::delete(&state, id).await {
        Ok(_) => {
            info!("User deleted! ID: {id}");
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!("Error deleting user with ID {id}: {e}");
            Err(e)
        }
    }
}
