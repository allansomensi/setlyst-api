use crate::database::AppState;
use crate::models::auth::access::AccessControl;
use crate::models::user::{Role, UserPublic};
use crate::models::{
    DeletePayload,
    user::{CreateUserPayload, UpdateUserPayload},
};
use crate::validations::{existence::user_exists, uniqueness::is_user_unique};
use crate::{errors::api_error::ApiError, models::user::User};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

/// Retrieves the total count of users.
///
/// This endpoint counts all users stored in the database and returns the count as an integer.
/// If no users are found, 0 is returned.
#[utoipa::path(
    get,
    path = "/api/v1/users/count",
    tags = ["Users"],
    summary = "Get the total count of users.",
    description = "This endpoint retrieves the total number of users stored in the database.",
    security(
        (),
        ("jwt_token" = ["jwt_token"])
    ),
    responses(
        (status = 200, description = "User count retrieved successfully.", body = i32),
        (status = 500, description = "An error occurred while retrieving the user count.")
    )
)]
pub async fn count_users(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve user count.");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match User::count(&state).await {
        Ok(count) => {
            info!("Successfully retrieved user count: {count}");
            Ok(Json(count))
        }
        Err(e) => {
            error!("Failed to retrieve user count: {e}");
            Err(e)
        }
    }
}

/// Retrieves a list of all users.
///
/// This endpoint fetches all users stored in the database.
/// If there are no users, returns an empty array.
#[utoipa::path(
    get,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "List all users.",
    description = "Fetches all users stored in the database. If there are no users, returns an empty array.",
    security(
        (),
        ("jwt_token" = ["jwt_token"])
    ),
    responses(
        (status = 200, description = "Users retrieved successfully.", body = Vec<UserPublic>),
        (status = 404, description = "No users found in the database."),
        (status = 500, description = "An error occurred while retrieving the users.")
    )
)]
pub async fn find_all_users(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve all users.");

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match User::find_all(&state).await {
        Ok(users) => {
            info!("Users listed successfully.");
            Ok(Json(users))
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
        ("jwt_token" = ["jwt_token"])
    ),
    responses(
        (status = 200, description = "User retrieved successfully.", body = UserPublic),
        (status = 404, description = "No user found with the specified ID."),
        (status = 500, description = "An error occurred while retrieving the user.")
    )
)]
pub async fn find_user_by_id(
    Path(id): Path<Uuid>,
    State(state): State<Arc<AppState>>,
    access: AccessControl,
) -> impl IntoResponse {
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
        ("jwt_token" = ["jwt_token"])
    ),
    responses(
        (status = 201, description = "User created successfully.", body = Uuid),
        (status = 400, description = "Invalid input, including empty name or name too short/long."),
        (status = 409, description = "Conflict: User with the same name already exists."),
        (status = 500, description = "An error occurred while creating the user.")
    )
)]
pub async fn create_user(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<CreateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(
        "Received request to create user with username: {}",
        payload.username
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    // Validations
    payload.validate()?;
    is_user_unique(&state, &payload.username).await?;

    match User::create(&state, &payload).await {
        Ok(new_user) => {
            info!("User created! ID: {}", &new_user.id);
            Ok((StatusCode::CREATED, Json(new_user.id)))
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
/// It accepts the user ID and the new details for the user.
/// The endpoint validates the new name to ensure it is not empty,
/// does not conflict with an existing user's name, and meets length requirements.
/// If the user is successfully updated, it returns the UUID of the updated user.
#[utoipa::path(
    put,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "Update an existing user.",
    description = "This endpoint updates the details of an existing user in the database.",
    request_body = UpdateUserPayload,
    security(
        (),
        ("jwt_token" = ["jwt_token"])
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
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<UpdateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to update user with ID: {}", payload.id);

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    // Validations
    payload.validate()?;
    user_exists(&state, payload.id).await?;

    match User::update(&state, &payload).await {
        Ok(user_id) => {
            info!("User updated! ID: {user_id}");
            Ok(Json(user_id))
        }
        Err(e) => {
            error!("Error updating user with ID {}: {e}", payload.id);
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
     path = "/api/v1/users",
     tags = ["Users"],
     summary = "Delete an existing user.",
     description = "This endpoint deletes a specific user from the database using its ID.",
     request_body = DeletePayload,
     security(
        (),
        ("jwt_token" = ["jwt_token"])
    ),
     responses(
         (status = 204, description = "User deleted successfully"),
         (status = 404, description = "User ID not found"),
         (status = 500, description = "An error occurred while deleting the user")
     )
 )]
pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    access: AccessControl,
    Json(payload): Json<DeletePayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to delete user with ID: {}", payload.id);

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    // Validations
    user_exists(&state, payload.id).await?;

    match User::delete(&state, &payload).await {
        Ok(_) => {
            info!("User deleted! ID: {}", &payload.id);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!("Error deleting user with ID {}: {e}", payload.id);
            Err(e)
        }
    }
}
