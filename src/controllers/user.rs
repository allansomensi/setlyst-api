use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse, PaginationQuery,
        admin::AdminUserOverview,
        audit::actions,
        auth::{
            ImpersonationResponse,
            access::{AccessControl, ClientIp},
        },
        notification::Notification,
        quota::{QuotaReport, UpdateUserQuotaPayload, UserQuotaSettings},
        user::{
            AdminResetPasswordPayload, BanUserPayload, ChangePasswordPayload,
            ChangePasswordResponse, CreateUserPayload, Role, Status, UpdateCurrentUserPayload,
            UpdateUserPayload, UserListQuery, UserProfileView, UserPublic, UsernameAvailability,
            UsernameHistoryEntry,
        },
        user_preferences::{SUPPORTED_LANGUAGES, UpdatePreferencesPayload, UserPreferences},
    },
    utils::{hashing, jwt::generate_impersonation_jwt},
    validations::{password::password_issues, username::validate_username},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde_json::json;
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

/// How long a user must wait before changing their username again.
const USERNAME_CHANGE_COOLDOWN_DAYS: i64 = 90;

/// Reads the locale the frontend is currently rendering with, sent as
/// `x-app-locale` on every server-side API call. Used only as the
/// *default* shown for a user who hasn't saved preferences yet.
fn fallback_language_from_headers(headers: &HeaderMap) -> String {
    headers
        .get("x-app-locale")
        .and_then(|v| v.to_str().ok())
        .filter(|locale| SUPPORTED_LANGUAGES.contains(locale))
        .unwrap_or("en")
        .to_string()
}

/// Loads the account a staff action targets, refusing when it's the
/// caller themselves or someone the caller doesn't outrank. This is the
/// single rule behind "moderators can't touch admins" (or each other).
async fn load_managed_target(
    state: &AppState,
    access: &AccessControl,
    target_id: Uuid,
) -> Result<UserPublic, ApiError> {
    access.require_staff()?;

    if target_id == access.user_id() {
        return Err(ApiError::cannot_target_self(
            "You can't perform this action on your own account.",
        ));
    }

    let target = state
        .user_repo
        .find_by_id(target_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if !access.role().outranks(&target.role) {
        return Err(ApiError::insufficient_role(match access.role() {
            Role::Moderator => "Moderators can only manage regular users.",
            _ => "You can't manage an account with the same or a higher role.",
        }));
    }

    Ok(target)
}

#[utoipa::path(
    get,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "List all users",
    description = "Returns a paginated list of all users, optionally filtered by `q` (username, email or name). Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(UserListQuery),
    responses(
        (status = 200, description = "Users listed successfully", body = PaginatedResponse<UserPublic>),
        (status = 403, description = "Forbidden - Insufficient permissions")
    )
)]
pub async fn find_all_users(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<UserListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = PaginationQuery {
        page: query.page,
        per_page: query.per_page,
    }
    .resolve();
    let search = query.search_pattern();

    let (users, total_items) = state
        .user_repo
        .find_all(page, per_page, search.as_deref())
        .await?;
    Ok(Json(PaginatedResponse::new(
        users,
        total_items,
        page,
        per_page,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Get user by ID",
    description = "Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "User found", body = UserPublic),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
    )
)]
pub async fn find_user_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;

    state
        .user_repo
        .find_by_id(id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/overview",
    tags = ["Users"],
    summary = "Staff overview of an account",
    description = "The account plus its resource usage against quotas, its quota settings and its band memberships. Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Overview retrieved.", body = AdminUserOverview),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_user_overview(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;

    let user = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let (usage, quota_settings, bands) = tokio::try_join!(
        state.quota_repo.report(id),
        state.quota_repo.get_user_settings(id),
        state.admin_repo.user_bands(id),
    )?;

    Ok(Json(AdminUserOverview {
        user,
        usage,
        quota_settings,
        bands,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/profile",
    tags = ["Users"],
    summary = "View another user's profile.",
    description = "Any authenticated user can view any other user's basic profile. Staff additionally see privileged details.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Profile retrieved successfully.", body = UserProfileView),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_user_profile(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(user.into_profile_view(access.is_staff())))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/username-history",
    tags = ["Users"],
    summary = "Get a user's past usernames",
    description = "Every username this user has previously held, oldest first. Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Username history retrieved successfully.", body = [UsernameHistoryEntry]),
        (status = 403, description = "Forbidden"),
        (status = 404, description = "User not found")
    )
)]
pub async fn get_username_history(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    state.user_repo.exists(id).await?;

    let history = state.user_repo.get_username_history(id).await?;
    Ok(Json(history))
}

#[utoipa::path(
    post,
    path = "/api/v1/users",
    tags = ["Users"],
    summary = "Create a new user",
    description = "Requires Admin or Moderator role. Moderators can only create regular users. Staff-created accounts must change their password at first sign-in unless `require_password_change` is `false`.",
    security(("jwt_token" = [])),
    request_body = CreateUserPayload,
    responses(
        (status = 201, description = "User created successfully", body = UserPublic),
        (status = 400, description = "Validation error or weak password"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 409, description = "Username already exists")
    )
)]
pub async fn create_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;

    let role = payload.role.clone().unwrap_or_default();
    // Staff may create accounts below their own rank; admins may also
    // create fellow admins.
    let allowed = access.role().outranks(&role) || (access.is_admin() && role == Role::Admin);
    if !allowed {
        return Err(ApiError::insufficient_role(
            "You can't create an account with this role.",
        ));
    }

    let issues = password_issues(&payload.password, Some(&payload.username));
    if !issues.is_empty() {
        return Err(ApiError::weak_password(&issues));
    }

    payload.validate()?;
    state.user_repo.is_unique(&payload.username, None).await?;

    let must_change = payload.require_password_change.unwrap_or(true);
    let new_user = state
        .user_repo
        .create(&payload, Some(access.user_id()), must_change)
        .await?;

    AuditEvent::by(&access, actions::USER_CREATED)
        .target("user", new_user.id, &new_user.username)
        .meta(json!({ "role": new_user.role, "must_change_password": must_change }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    info!(requester_id = %access.user_id(), new_user_id = %new_user.id, "User created");

    let mut headers = HeaderMap::new();
    if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/users/{}", new_user.id)) {
        headers.insert(LOCATION, location);
    }

    let public = state
        .user_repo
        .find_by_id(new_user.id)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok((StatusCode::CREATED, headers, Json(public)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Update a user",
    description = "Requires Admin or Moderator role, and the caller must outrank the target. Only admins can change roles. Passwords are changed through `/users/{id}/password-reset`.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = UpdateUserPayload,
    responses(
        (status = 200, description = "User updated successfully", body = UserPublic),
        (status = 400, description = "Validation error"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
    )
)]
pub async fn update_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;

    // Role changes are admin-only. An admin may also demote another admin
    // (the one exception to "never manage a peer"), so a compromised or
    // departing admin can be stepped down without database access — as
    // long as at least one other active admin remains.
    let role_change_on_peer_admin = access.is_admin()
        && payload.role.is_some()
        && id != access.user_id()
        && state
            .user_repo
            .find_by_id(id)
            .await?
            .is_some_and(|u| u.role == Role::Admin);

    let target = if role_change_on_peer_admin {
        if payload.username.is_some()
            || payload.email.is_some()
            || payload.first_name.is_some()
            || payload.last_name.is_some()
            || payload.status.is_some()
        {
            return Err(ApiError::insufficient_role(
                "Only the role of another admin can be changed. Demote them first to edit anything else.",
            ));
        }
        state
            .user_repo
            .find_by_id(id)
            .await?
            .ok_or(ApiError::NotFound)?
    } else {
        load_managed_target(&state, &access, id).await?
    };

    if let Some(new_role) = &payload.role
        && *new_role != target.role
    {
        access
            .require_admin()
            .map_err(|_| ApiError::insufficient_role("Only admins can change platform roles."))?;

        if target.role == Role::Admin && state.user_repo.count_active_admins().await? <= 1 {
            return Err(ApiError::rule(
                StatusCode::CONFLICT,
                codes::LAST_ADMIN,
                "The platform must keep at least one active admin.",
            ));
        }
    }

    if let Some(username) = &payload.username {
        state.user_repo.is_unique(username, Some(id)).await?;
    }

    state
        .user_repo
        .update(id, &payload, Some(access.user_id()))
        .await?;

    let label = payload.username.as_deref().unwrap_or(&target.username);

    if let Some(new_role) = &payload.role
        && *new_role != target.role
    {
        let notification = Notification::platform_role_changed(
            id,
            target.role.clone(),
            new_role.clone(),
            access.user_id(),
        );
        if let Err(e) = state.notification_repo.create(&notification).await {
            error!(target_user_id = %id, error = %e, "Failed to create role change notification");
        }

        AuditEvent::by(&access, actions::USER_ROLE_CHANGED)
            .target("user", id, label)
            .meta(json!({ "from": target.role, "to": new_role }))
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }

    if let Some(status) = &payload.status
        && *status != target.status
    {
        let action = match status {
            Status::Active => actions::USER_ACTIVATED,
            Status::Inactive => actions::USER_DEACTIVATED,
        };
        AuditEvent::by(&access, action)
            .target("user", id, label)
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }

    let profile_changed = payload.username.is_some()
        || payload.email.is_some()
        || payload.first_name.is_some()
        || payload.last_name.is_some();
    if profile_changed {
        let fields: Vec<&str> = [
            payload.username.as_ref().map(|_| "username"),
            payload.email.as_ref().map(|_| "email"),
            payload.first_name.as_ref().map(|_| "first_name"),
            payload.last_name.as_ref().map(|_| "last_name"),
        ]
        .into_iter()
        .flatten()
        .collect();
        let previous_username = payload.username.as_ref().map(|_| &target.username);

        AuditEvent::by(&access, actions::USER_UPDATED)
            .target("user", id, label)
            .meta(json!({
                "fields": fields,
                "previous_username": previous_username,
            }))
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }

    let updated = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(updated))
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}",
    tags = ["Users"],
    summary = "Permanently delete a user",
    description = "Irreversible — prefer deactivation or a suspension. The caller must outrank the target (moderators can only delete regular users; admins can never be deleted, only demoted first). Bands the user owned are handed to their most senior remaining member.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 204, description = "User deleted successfully"),
        (status = 403, description = "Forbidden - Insufficient permissions"),
        (status = 404, description = "User not found")
    )
)]
pub async fn delete_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let target = load_managed_target(&state, &access, id).await?;

    state.user_repo.delete(id).await?;

    AuditEvent::by(&access, actions::USER_DELETED)
        .target("user", id, &target.username)
        .meta(json!({ "role": target.role, "email": target.email }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    info!(requester_id = %access.user_id(), target_user_id = %id, "User permanently deleted");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/ban",
    tags = ["Users"],
    summary = "Suspend a user",
    description = "Suspends the account for `duration_hours` (or permanently when omitted) and signs it out everywhere. The caller must outrank the target.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = BanUserPayload,
    responses(
        (status = 200, description = "User suspended.", body = UserPublic),
        (status = 403, description = "Forbidden")
    )
)]
pub async fn ban_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<BanUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let target = load_managed_target(&state, &access, id).await?;

    let until = payload
        .duration_hours
        .map(|hours| Utc::now().naive_utc() + Duration::hours(hours));
    let reason = payload
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());

    state
        .user_repo
        .ban(id, until, reason, access.user_id())
        .await?;

    AuditEvent::by(&access, actions::USER_BANNED)
        .target("user", id, &target.username)
        .meta(json!({ "until": until, "reason": reason, "duration_hours": payload.duration_hours }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    let updated = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(updated))
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/{id}/ban",
    tags = ["Users"],
    summary = "Lift a suspension",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Suspension lifted.", body = UserPublic),
        (status = 403, description = "Forbidden")
    )
)]
pub async fn unban_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let target = load_managed_target(&state, &access, id).await?;

    state.user_repo.unban(id, access.user_id()).await?;

    AuditEvent::by(&access, actions::USER_UNBANNED)
        .target("user", id, &target.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    let updated = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(updated))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/password-reset",
    tags = ["Users"],
    summary = "Set a temporary password for a user",
    description = "Replaces the password, signs the account out everywhere and (by default) forces a change at next sign-in. The caller must outrank the target.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = AdminResetPasswordPayload,
    responses(
        (status = 204, description = "Password reset."),
        (status = 400, description = "Weak password"),
        (status = 403, description = "Forbidden")
    )
)]
pub async fn reset_user_password(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<AdminResetPasswordPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let target = load_managed_target(&state, &access, id).await?;

    let issues = password_issues(&payload.new_password, Some(&target.username));
    if !issues.is_empty() {
        return Err(ApiError::weak_password(&issues));
    }
    payload.validate()?;

    let require_change = payload.require_change.unwrap_or(true);
    state
        .user_repo
        .set_password(
            id,
            &payload.new_password,
            require_change,
            true,
            Some(access.user_id()),
        )
        .await?;

    AuditEvent::by(&access, actions::USER_PASSWORD_RESET)
        .target("user", id, &target.username)
        .meta(json!({ "require_change": require_change }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/sessions/revoke",
    tags = ["Users"],
    summary = "Sign a user out everywhere",
    description = "Invalidates every token issued so far for the account. The caller must outrank the target.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses((status = 204, description = "Sessions revoked."))
)]
pub async fn revoke_user_sessions(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let target = load_managed_target(&state, &access, id).await?;
    state.user_repo.revoke_sessions(id).await?;

    AuditEvent::by(&access, actions::USER_SESSIONS_REVOKED)
        .target("user", id, &target.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/users/{id}/impersonate",
    tags = ["Users"],
    summary = "View the platform as another user",
    description = "Issues a short-lived, strictly read-only token acting as the target (every write answers `IMPERSONATION_READ_ONLY`). The caller must outrank the target. Every impersonation is recorded in the audit log.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Impersonation token issued.", body = ImpersonationResponse),
        (status = 403, description = "Forbidden")
    )
)]
pub async fn impersonate_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    if access.impersonator().is_some() {
        return Err(ApiError::impersonation_read_only());
    }

    load_managed_target(&state, &access, id).await?;

    let account = state
        .user_repo
        .find_account(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    let (token, expires_at) = generate_impersonation_jwt(&account, access.user_id())?;

    AuditEvent::by(&access, actions::USER_IMPERSONATED)
        .target("user", id, &account.username)
        .meta(json!({ "expires_at": expires_at }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    info!(requester_id = %access.user_id(), target_user_id = %id, "Impersonation started");

    Ok(Json(ImpersonationResponse {
        token,
        expires_at,
        user_id: account.id,
        username: account.username,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/quotas",
    tags = ["Users"],
    summary = "Get a user's quota settings",
    description = "Per-user overrides of the platform limits. Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses((status = 200, description = "Quota settings.", body = UserQuotaSettings))
)]
pub async fn get_user_quotas(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    state.user_repo.exists(id).await?;
    Ok(Json(state.quota_repo.get_user_settings(id).await?))
}

#[utoipa::path(
    put,
    path = "/api/v1/users/{id}/quotas",
    tags = ["Users"],
    summary = "Set a user's quota overrides",
    description = "Replaces the user's overrides (null = platform default) and the `unlimited` flag. Admin only.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = UpdateUserQuotaPayload,
    responses((status = 200, description = "Updated usage report.", body = QuotaReport))
)]
pub async fn update_user_quotas(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateUserQuotaPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;

    let target = state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state
        .quota_repo
        .set_user_settings(id, &payload.overrides, payload.unlimited, access.user_id())
        .await?;

    AuditEvent::by(&access, actions::USER_QUOTAS_UPDATED)
        .target("user", id, &target.username)
        .meta(json!({ "overrides": payload.overrides, "unlimited": payload.unlimited }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(Json(state.quota_repo.report(id).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me",
    tags = ["Users"],
    summary = "Get current user profile",
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "User profile retrieved successfully", body = UserPublic),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn get_current_user(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user = state
        .user_repo
        .find_by_id(access.user_id())
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok((StatusCode::OK, Json(user)))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/quotas",
    tags = ["Users"],
    summary = "Current user's usage and limits",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Usage report.", body = QuotaReport))
)]
pub async fn get_current_user_quotas(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.quota_repo.report(access.user_id()).await?))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me",
    tags = ["Users"],
    summary = "Update current user profile",
    description = "Username changes are limited to one every 90 days (`USERNAME_COOLDOWN`). Empty strings clear optional fields.",
    request_body = UpdateCurrentUserPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Profile updated successfully", body = UserPublic),
        (status = 400, description = "Invalid input")
    )
)]
pub async fn update_current_user(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<UpdateCurrentUserPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, "Processing request to update current user profile");

    payload.validate()?;

    if let Some(new_username) = &payload.username {
        state
            .user_repo
            .is_unique(new_username, Some(user_id))
            .await?;

        let current = state
            .user_repo
            .find_by_id(user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        // Only a byte-for-byte identical resubmission is a no-op; even a
        // case-only change is a real rename (it changes the displayed name).
        if new_username.trim() != current.username
            && let Some(changed_at) = current.username_changed_at
        {
            let eligible_at = changed_at + Duration::days(USERNAME_CHANGE_COOLDOWN_DAYS);
            if Utc::now().naive_utc() < eligible_at {
                return Err(ApiError::rule_with_meta(
                    StatusCode::BAD_REQUEST,
                    codes::USERNAME_COOLDOWN,
                    format!(
                        "You can change your username again on {}.",
                        eligible_at.format("%Y-%m-%d")
                    ),
                    json!({ "eligible_at": eligible_at }),
                ));
            }
        }
    }

    let payload = UpdateUserPayload::from(payload);
    match state
        .user_repo
        .update(user_id, &payload, Some(user_id))
        .await
    {
        Ok(_) | Err(ApiError::NotModified) => {}
        Err(e) => return Err(e),
    }

    let user = state
        .user_repo
        .find_by_id(user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::OK, Json(user)))
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
    description = "Case-insensitive live check. Invalid or reserved names report `available: false` with a `reason`. The caller's own current username always reports as available.",
    params(UsernameAvailabilityQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Availability checked.", body = UsernameAvailability))
)]
pub async fn check_username_availability(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<UsernameAvailabilityQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let username = query.username.trim();

    if let Err(e) = validate_username(username) {
        return Ok(Json(UsernameAvailability {
            available: false,
            reason: e.message.map(|m| m.to_string()),
        }));
    }

    let available = state
        .user_repo
        .is_username_available(username, Some(access.user_id()))
        .await?;

    Ok(Json(UsernameAvailability {
        available,
        reason: None,
    }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me/password",
    tags = ["Users"],
    summary = "Change current user password",
    description = "Requires the current password. The new one must satisfy the policy and differ from the current one. Every session — including the caller's — is revoked, so the client must sign in again.",
    request_body = ChangePasswordPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Password updated successfully", body = ChangePasswordResponse),
        (status = 400, description = "Weak or reused password"),
        (status = 401, description = "Incorrect current password")
    )
)]
pub async fn change_current_user_password(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<ChangePasswordPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    // Looked up by ID, not by the username in the token: the username can
    // have changed since the token was issued, which used to make this
    // endpoint fail with "not found" right after a rename.
    let user = state
        .user_repo
        .find_account(user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    hashing::verify_password(&payload.current_password, &user.password_hash)?;

    if payload.current_password == payload.new_password {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::PASSWORD_REUSED,
            "The new password must be different from the current one.",
        ));
    }

    let issues = password_issues(&payload.new_password, Some(&user.username));
    if !issues.is_empty() {
        return Err(ApiError::weak_password(&issues));
    }
    payload.validate()?;

    state
        .user_repo
        .set_password(user_id, &payload.new_password, false, true, Some(user_id))
        .await?;

    AuditEvent::by(&access, actions::USER_PASSWORD_CHANGED)
        .target("user", user_id, &user.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    info!(%user_id, "Password changed; sessions revoked");

    Ok((
        StatusCode::OK,
        Json(ChangePasswordResponse {
            reauth_required: true,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/preferences",
    tags = ["Users"],
    summary = "Get current user preferences",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Preferences retrieved successfully", body = UserPreferences))
)]
pub async fn get_current_user_preferences(
    State(state): State<AppState>,
    access: AccessControl,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let fallback_language = fallback_language_from_headers(&headers);

    let prefs = state
        .user_prefs_repo
        .get_by_user_id(access.user_id(), &fallback_language)
        .await?;
    Ok((StatusCode::OK, Json(prefs)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/users/me/preferences",
    tags = ["Users"],
    summary = "Update current user preferences",
    description = "`ui_settings` is shallow-merged: keys present replace stored ones, `null` removes a key.",
    request_body = UpdatePreferencesPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Preferences updated successfully", body = UserPreferences),
        (status = 400, description = "Validation error")
    )
)]
pub async fn update_current_user_preferences(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<UpdatePreferencesPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;

    let prefs = state
        .user_prefs_repo
        .upsert(access.user_id(), &payload)
        .await?;
    Ok((StatusCode::OK, Json(prefs)))
}

#[utoipa::path(
    get,
    path = "/api/v1/users/{id}/preferences",
    tags = ["Users"],
    summary = "Get user preferences by ID",
    description = "Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    params(("id" = Uuid, Path, description = "User UUID")),
    responses((status = 200, description = "Preferences found", body = UserPreferences))
)]
pub async fn get_user_preferences_by_id(
    Path(id): Path<Uuid>,
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    state.user_repo.exists(id).await?;

    let prefs = state.user_prefs_repo.get_by_user_id(id, "en").await?;
    Ok((StatusCode::OK, Json(prefs)))
}
