use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        user::{
            ChangePasswordPayload, CreateUserPayload, Role, UpdateCurrentUserPayload,
            UpdateUserPayload, User, UserProfileView, UserPublic, UsernameAvailability,
            UsernameHistoryEntry,
        },
        user_preferences::{UpdatePreferencesPayload, UserPreferences},
    },
    utils::hashing,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

/// How long a user must wait before changing their username again.
const USERNAME_CHANGE_COOLDOWN_DAYS: i64 = 90;

/// The languages the frontend actually ships (see `i18n/routing.ts`).
const SUPPORTED_LANGUAGES: [&str; 3] = ["en", "pt-BR", "es"];

/// Reads the locale the frontend is currently rendering with, sent as
/// `x-app-locale` on every server-side API call (see `fetchServerApi`).
/// Used only as the *default* shown for a user who hasn't saved
/// preferences yet — never overrides an already-saved preference.
fn fallback_language_from_headers(headers: &HeaderMap) -> String {
    headers
        .get("x-app-locale")
        .and_then(|v| v.to_str().ok())
        .filter(|locale| SUPPORTED_LANGUAGES.contains(locale))
        .unwrap_or("en")
        .to_string()
}

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
    get,
    path = "/api/v1/users/{id}/profile",
    tags = ["Users"],
    summary = "View another user's profile.",
    description = "Any authenticated user can view any other user's basic profile (username, name, join date). Admins additionally see privileged details (email, role, status, last username change).",
    security(
        (),
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    responses(
        (status = 200, description = "Profile retrieved successfully.", body = UserProfileView),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_user_profile(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();
    let viewer_is_admin = access.0.role == Role::Admin;

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to view user profile"
    );

    let user = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    info!(%requester_id, target_user_id = %id, "User profile retrieved successfully");
    Ok(Json(user.into_profile_view(viewer_is_admin)))
}

#[utoipa::path(
get,
path = "/api/v1/users/{id}/username-history",
    tags = ["Users"],
    summary = "Get a user's past usernames",
    description = "Returns every username this user has previously held, oldest first. Requires Admin role.",
    security(
        (),
        ("jwt_token" = [])
    ),
    params(
        ("id" = Uuid, Path, description = "User UUID")
    ),
    responses(
        (status = 200, description = "Username history retrieved successfully.", body = [UsernameHistoryEntry]),
        (status = 401, description = "Unauthorized"),
        (status = 403, description = "Forbidden - Admin role required"),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_username_history(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        target_user_id = %id,
        "Processing request to retrieve username history"
    );

    access.require_any_role(&[Role::Admin])?;
    state.user_repo.exists(id).await?;

    let history = state.user_repo.get_username_history(id).await?;

    info!(
        %requester_id,
        target_user_id = %id,
        "Username history retrieved successfully"
    );

    Ok(Json(history))
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

    if let Some(new_username) = &payload.username {
        state
            .user_repo
            .is_unique(new_username, Some(user_id))
            .await?;

        // Only a byte-for-byte identical resubmission is a no-op; even a
        // case-only change (e.g. "augusto" -> "Augusto") is treated as a
        // real rename here — it still updates the displayed name, so it
        // consumes the cooldown and gets recorded in history like any
        // other change. Uniqueness above is checked case-insensitively,
        // but that's a separate concern (is the name available at all).
        let current = state
            .user_repo
            .find_by_id(user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        if new_username != &current.username
            && let Some(changed_at) = current.username_changed_at
        {
            let eligible_at = changed_at + Duration::days(USERNAME_CHANGE_COOLDOWN_DAYS);
            let now = Utc::now().naive_utc();
            if now < eligible_at {
                return Err(ApiError::BadRequest(format!(
                    "You can change your username again on {}.",
                    eligible_at.format("%Y-%m-%d")
                )));
            }
        }
    }

    let payload = UpdateUserPayload::from(payload);

    state.user_repo.update(user_id, &payload).await?;

    info!(
        %user_id,
        "Current user profile updated successfully"
    );

    Ok((StatusCode::OK, Json("Profile updated successfully")))
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UsernameAvailabilityQuery {
    pub username: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/username-availability",
    tags = ["Users"],
    summary = "Check whether a username is available",
    description = "Case-insensitive live check for the Settings page's username field. The caller's own current username always reports as available.",
    params(UsernameAvailabilityQuery),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Availability checked successfully.", body = UsernameAvailability),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn check_username_availability(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<UsernameAvailabilityQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        username = %query.username,
        "Processing request to check username availability"
    );

    let available = state
        .user_repo
        .is_username_available(&query.username, Some(user_id))
        .await?;

    Ok(Json(UsernameAvailability { available }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me/password",
    tags = ["Users"],
    summary = "Change current user password",
    description = "Updates the password of the currently authenticated user. Requires the current password for security verification.",
    request_body = ChangePasswordPayload,
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Password updated successfully"),
        (status = 400, description = "Validation error or invalid input"),
        (status = 401, description = "Unauthorized or incorrect current password")
    )
)]
pub async fn change_current_user_password(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<ChangePasswordPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(
        %user_id,
        "Processing request to change current user password"
    );

    payload.validate()?;

    let user = state
        .user_repo
        .find_by_username(&access.0.username)
        .await?
        .ok_or(ApiError::NotFound)?;

    hashing::verify_password(&payload.current_password, &user.password_hash)?;

    let update_payload = UpdateUserPayload {
        username: None,
        email: None,
        password: Some(payload.new_password),
        first_name: None,
        last_name: None,
        role: None,
        status: None,
    };

    state.user_repo.update(user_id, &update_payload).await?;

    info!(
        %user_id,
        "Current user password updated successfully"
    );

    Ok((StatusCode::OK, Json("Password updated successfully")))
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
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let requester_id = access.user_id();

    debug!(
        %requester_id,
        "Processing request to retrieve current user preferences"
    );

    // The frontend sends the locale it's actually rendering with (see
    // `fetchServerApi`), so a user who hasn't saved preferences yet still
    // sees Settings default to the language they're already viewing,
    // instead of always falling back to English.
    let fallback_language = fallback_language_from_headers(&headers);

    match state
        .user_prefs_repo
        .get_by_user_id(requester_id, &fallback_language)
        .await
    {
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

    // No meaningful "current locale" to fall back to here — this is an
    // admin looking up someone else's preferences, not that user's own
    // Settings page — so just use the neutral default.
    match state.user_prefs_repo.get_by_user_id(id, "en").await {
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
