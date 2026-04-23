use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        user::{
            CreateUserPayload, Role, UpdateCurrentUserPayload, UpdateUserPayload, User, UserPublic,
        },
        user_preferences::{UpdatePreferencesPayload, UserPreferences},
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
        (),
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
    let requester_id = access.user_id();

    let current_page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(20).clamp(1, 100);

    debug!(
        %requester_id,
        current_page,
        per_page,
        "Processing request to retrieve paginated users"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match state.user_repo.find_all(current_page, per_page).await {
        Ok((users, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(
                %requester_id,
                total_items,
                total_pages,
                "Users retrieved successfully"
            );

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
            error!(
                %requester_id,
                error = %e,
                "Failed to retrieve users"
            );
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
        (),
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
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to retrieve user by ID"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match state.user_repo.find_by_id(id).await {
        Ok(Some(user)) => {
            info!(
                %requester_id,
                target_user_id = %id,
                "User retrieved successfully"
            );
            Ok(Json(user))
        }
        Ok(None) => {
            info!(
                %requester_id,
                target_user_id = %id,
                "User not found"
            );
            Err(ApiError::NotFound)
        }
        Err(e) => {
            error!(
                %requester_id,
                target_user_id = %id,
                error = %e,
                "Failed to retrieve user by ID"
            );
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
        (),
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
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        new_username = %payload.username,
        "Processing request to create a new user"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    payload.validate()?;
    state.user_repo.is_unique(&payload.username, None).await?;

    match state.user_repo.create(&payload).await {
        Ok(new_user) => {
            info!(
                %requester_id,
                new_user_id = %new_user.id,
                "User created successfully"
            );

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
                %requester_id,
                new_username = %payload.username,
                error = %e,
                "Failed to create user"
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
        (),
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
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to update user"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    payload.validate()?;
    state.user_repo.exists(id).await?;

    if let Some(username) = &payload.username {
        state.user_repo.is_unique(username, Some(id)).await?;
    }

    match state.user_repo.update(id, &payload).await {
        Ok(user_id) => {
            info!(
                %requester_id,
                target_user_id = %user_id,
                "User updated successfully"
            );
            Ok(Json(user_id))
        }
        Err(e) => {
            error!(
                %requester_id,
                target_user_id = %id,
                error = %e,
                "Failed to update user"
            );
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
        (),
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
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to delete user"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    state.user_repo.exists(id).await?;

    match state.user_repo.delete(id).await {
        Ok(_) => {
            info!(
                %requester_id,
                target_user_id = %id,
                "User deleted successfully"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Err(e) => {
            error!(
                %requester_id,
                target_user_id = %id,
                error = %e,
                "Failed to delete user"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me",
    tags = ["Users"],
    summary = "Get current user profile",
    description = "Retrieves the profile information of the currently authenticated user based on the JWT token.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "User profile retrieved successfully", body = User),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_current_user(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        "Processing request to retrieve current user profile"
    );

    let user = state
        .user_repo
        .find_by_id(user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    info!(
        %user_id,
        "Current user profile retrieved successfully"
    );

    Ok((StatusCode::OK, Json(user)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me",
    tags = ["Users"],
    summary = "Update current user profile",
    description = "Updates the profile details of the currently authenticated user.",
    request_body = UpdateCurrentUserPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Profile updated successfully"),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn update_current_user(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<UpdateCurrentUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        "Processing request to update current user profile"
    );

    payload.validate()?;

    let payload = UpdateUserPayload::from(payload);

    state.user_repo.update(user_id, &payload).await?;

    info!(
        %user_id,
        "Current user profile updated successfully"
    );

    Ok((StatusCode::OK, Json("Profile updated successfully")))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/preferences",
    tags = ["Users"],
    summary = "Get current user preferences",
    description = "Retrieves the application preferences of the currently authenticated user.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Preferences retrieved successfully", body = UserPreferences),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn get_current_user_preferences(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        "Processing request to retrieve current user preferences"
    );

    match state.user_prefs_repo.get_by_user_id(requester_id).await {
        Ok(prefs) => {
            info!(
                %requester_id,
                "Current user preferences retrieved successfully"
            );
            Ok((StatusCode::OK, Json(prefs)))
        }
        Err(e) => {
            error!(
                %requester_id,
                error = %e,
                "Failed to retrieve current user preferences"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me/preferences",
    tags = ["Users"],
    summary = "Update current user preferences",
    description = "Updates the application preferences of the currently authenticated user.",
    request_body = UpdatePreferencesPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Preferences updated successfully"),
        (status = 400, description = "Validation error"),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn update_current_user_preferences(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<UpdatePreferencesPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        "Processing request to update current user preferences"
    );

    payload.validate()?;

    match state.user_prefs_repo.upsert(requester_id, &payload).await {
        Ok(_) => {
            info!(
                %requester_id,
                "Current user preferences updated successfully"
            );
            Ok((StatusCode::OK, Json("Preferences updated successfully")))
        }
        Err(e) => {
            error!(
                %requester_id,
                error = %e,
                "Failed to update current user preferences"
            );
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/preferences",
    tags = ["Users"],
    summary = "Get user preferences by ID",
    description = "Returns the preferences for a specific user. Requires Admin or Moderator role.",
    security(
        (),
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    responses(
        (status = 200, description = "Preferences found", body = UserPreferences),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User preferences not found")
    )
)]
pub async fn get_user_preferences_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to retrieve user preferences by ID"
    );

    access.require_any_role(&[Role::Admin, Role::Moderator])?;

    match state.user_prefs_repo.get_by_user_id(id).await {
        Ok(prefs) => {
            info!(
                %requester_id,
                target_user_id = %id,
                "User preferences retrieved successfully"
            );
            Ok((StatusCode::OK, Json(prefs)))
        }
        Err(e) => {
            error!(
                %requester_id,
                target_user_id = %id,
                error = %e,
                "Failed to retrieve user preferences by ID"
            );
            Err(e)
        }
    }
}
