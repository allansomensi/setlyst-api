use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::{
        audit::actions,
        auth::{LoginPayload, LoginResponse, access::ClientIp, token::VerifyTokenPayload},
        user::{CreateUserPayload, RegisterPayload, Status, UserPublic},
    },
    utils::{
        hashing::{dummy_verify, verify_password},
        jwt::{decode_jwt, generate_jwt},
    },
    validations::password::{is_password_compliant, password_issues},
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde_json::json;
use tracing::{debug, info, warn};
use validator::Validate;

/// Returns a JWT if the credentials passed are valid.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tags = ["Auth"],
    summary = "Sign in and receive a JWT.",
    description = "Unknown usernames and wrong passwords are indistinguishable (`INVALID_CREDENTIALS`). Suspended or deactivated accounts are only reported after a correct password (`ACCOUNT_BANNED` / `ACCOUNT_DEACTIVATED`). When `must_change_password` is `true`, every other endpoint answers `PASSWORD_CHANGE_REQUIRED` until the password is changed.",
    request_body = LoginPayload,
    responses(
        (status = 200, description = "Logged in successfully.", body = LoginResponse),
        (status = 401, description = "Invalid credentials."),
        (status = 403, description = "Account suspended or deactivated."),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(payload): Json<LoginPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received login request");

    if payload.validate().is_err() {
        return Err(ApiError::invalid_credentials());
    }

    let Some(user) = state.user_repo.find_by_username(&payload.username).await? else {
        // Spend the same time as a real check so the response can't be
        // used to probe which usernames exist.
        dummy_verify(&payload.password);
        return Err(ApiError::invalid_credentials());
    };

    if verify_password(&payload.password, &user.password_hash).is_err() {
        AuditEvent::new(actions::USER_LOGIN_FAILED)
            .target("user", user.id, &user.username)
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
        return Err(ApiError::invalid_credentials());
    }

    // Account state is only revealed to someone who proved they know the
    // password — otherwise these messages would confirm the account exists.
    if user.status != Status::Active {
        return Err(ApiError::account_deactivated());
    }
    if let Some(until) = user.active_ban() {
        return Err(ApiError::account_banned(until, user.ban_reason.clone()));
    }

    // A correct password that no longer meets the policy (set before it
    // was tightened) still signs in, but the account is walked through a
    // mandatory change before it can do anything else.
    let mut must_change_password = user.must_change_password;
    if !must_change_password && !is_password_compliant(&payload.password, Some(&user.username)) {
        state
            .user_repo
            .set_must_change_password(user.id, true)
            .await?;
        must_change_password = true;
        info!(user_id = %user.id, "Legacy password below policy; change required");
    }

    let token = generate_jwt(&user)?;
    let is_first_login = state.user_repo.mark_login(user.id).await?;

    info!(user_id = %user.id, "Login successful");

    Ok((
        StatusCode::OK,
        Json(LoginResponse {
            token,
            is_first_login,
            must_change_password,
        }),
    ))
}

/// Register a new user.
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tags = ["Auth"],
    summary = "Register a new user.",
    description = "Creates a regular account. The password must satisfy the platform policy (`WEAK_PASSWORD` lists the failed rules in `meta.issues`).",
    request_body = RegisterPayload,
    responses(
        (status = 201, description = "User registered successfully.", body = UserPublic),
        (status = 400, description = "Invalid input or weak password."),
        (status = 409, description = "Username already taken.")
    )
)]
pub async fn register(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(payload): Json<RegisterPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received registration request");

    let issues = password_issues(&payload.password, Some(&payload.username));
    if !issues.is_empty() {
        return Err(ApiError::weak_password(&issues));
    }

    payload.validate()?;
    state.user_repo.is_unique(&payload.username, None).await?;

    let user_payload = CreateUserPayload::from(payload);
    let new_user = state.user_repo.create(&user_payload, None, false).await?;

    AuditEvent::new(actions::USER_REGISTERED)
        .actor(new_user.id, &new_user.username)
        .target("user", new_user.id, &new_user.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    info!(user_id = %new_user.id, "User registered");

    let public = state
        .user_repo
        .find_by_id(new_user.id)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok((StatusCode::CREATED, Json(public)))
}

/// Checks if a JWT is valid.
#[utoipa::path(
    post,
    path = "/api/v1/auth/verify",
    tags = ["Auth"],
    summary = "Verify a JWT.",
    description = "Checks the signature, the expiry *and* that the token hasn't been revoked (password change, suspension, deactivation, sign-out everywhere).",
    request_body = VerifyTokenPayload,
    responses(
        (status = 200, description = "Token is valid."),
        (status = 401, description = "Token is invalid, expired or revoked.")
    )
)]
pub async fn verify(
    State(state): State<AppState>,
    Json(payload): Json<VerifyTokenPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let claims = decode_jwt(payload.token)
        .map_err(|_| ApiError::session_revoked())?
        .claims;

    let account = state
        .user_repo
        .auth_state(claims.sub)
        .await?
        .ok_or_else(ApiError::session_revoked)?;

    let banned = crate::models::user::active_ban(
        account.banned_at,
        account.banned_until,
        chrono::Utc::now().naive_utc(),
    )
    .is_some();

    if claims.imp.is_none()
        && (account.token_version != claims.ver || account.status != Status::Active || banned)
    {
        warn!(user_id = %claims.sub, "Rejected a revoked token at /auth/verify");
        return Err(ApiError::session_revoked());
    }

    Ok((StatusCode::OK, Json(json!({ "valid": true }))))
}
