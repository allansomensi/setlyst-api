use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        user::{CreateUserPayload, Role, UpdateUserPayload, UserPublic},
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
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "List all users",
    description = "Returns a paginated list of all users. Requires Admin or Moderator role.",
    security(
        ("jwt_token" = [])
    ),
    params(
        PaginationQuery
    ),
    responses(
        (status = 200, description = "Users listed successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions")
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

    match state.user_repo.find_all(current_page, per_page).await {
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

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Get user by ID",
    description = "Returns a single user by their UUID. Requires Admin or Moderator role.",
    security(
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    responses(
        (status = 200, description = "User found"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
    )
)]
pub async fn find_user_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to retrieve user with id: {id}");
    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match state.user_repo.find_by_id(id).await {
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

#[utoipa::path(
    post,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "Create a new user",
    description = "Creates a new user. Requires Admin or Moderator role.",
    security(
        ("jwt_token" = [])
    ),
    request_body = CreateUserPayload,
    responses(
        (status = 201, description = "User created successfully"),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 409, description = "Username already exists")
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
    state.user_repo.is_unique(&payload.username, None).await?;

    match state.user_repo.create(&payload).await {
        Ok(new_user) => {
            info!("User created! ID: {}", &new_user.id);

            let mut headers = HeaderMap::new();
            let location = format!("/api/v1/users/{}", new_user.id);
            if let Ok(header_value) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, header_value);
            }

            Ok((
                StatusCode::CREATED,
                headers,
                Json(UserPublic::from(new_user)),
            ))
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

#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Update a user",
    description = "Updates an existing user. Requires Admin or Moderator role.",
    security(
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    request_body = UpdateUserPayload,
    responses(
        (status = 200, description = "User updated successfully"),
        (status = 304, description = "Not modified"),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
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
    state.user_repo.exists(id).await?;

    if let Some(username) = &payload.username {
        state.user_repo.is_unique(username, Some(id)).await?;
    }

    match state.user_repo.update(id, &payload).await {
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

#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Delete a user",
    description = "Deletes an existing user. Requires Admin or Moderator role.",
    security(
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    responses(
        (status = 204, description = "User deleted successfully"),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
    )
)]
pub async fn delete_user(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received request to delete user with ID: {id}");
    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    state.user_repo.exists(id).await?;

    match state.user_repo.delete(id).await {
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
