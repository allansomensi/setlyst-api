//! Self-service account security under `/users/me`: e-mail verification
//! and change, two-factor authentication, linked sign-in providers,
//! communication preferences, consent and account deletion.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    email::{EmailTemplate, OutgoingEmail, enqueue, outbox::mask_email},
    errors::api_error::{ApiError, codes},
    models::{
        audit::actions,
        auth::{
            ReauthRequiredResponse,
            access::{AccessControl, ClientIp},
        },
        communication::{Category, CommunicationSettings, UpdateCommunicationPayload},
        notification::Notification,
        security::{
            CodeSentResponse, EmailChangePayload, EmailCodePayload, LinkedIdentity,
            PasswordConfirmationPayload, RecoveryCodesResponse, SecurityOverview,
            TwoFactorCodePayload, TwoFactorDisablePayload, TwoFactorSetupResponse,
            VerificationPurpose,
        },
        user::{
            AcceptTermsPayload, CURRENT_TERMS_VERSION, DeleteAccountPayload, User, UserPublic,
            normalize_email,
        },
    },
    services::{
        account::{self, CODE_TTL_MINUTES, TOTP_SETUP_TTL_MINUTES, invalid_two_factor_code},
        billing,
        notifier::notify,
    },
    utils::{
        codes::{RECOVERY_CODE_COUNT, recovery_code},
        crypto::{encrypt, hash_code, random_bytes},
        hashing::verify_password_async,
        totp,
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde_json::json;
use tracing::{error, info};
use validator::Validate;

async fn current_account(state: &AppState, access: &AccessControl) -> Result<User, ApiError> {
    state
        .user_repo
        .find_account(access.user_id())
        .await?
        .ok_or(ApiError::NotFound)
}

/// Requires the current password when the account has one (Google-only
/// accounts have none to confirm).
async fn confirm_password(user: &User, password: Option<&str>) -> Result<(), ApiError> {
    if !user.password_set {
        return Ok(());
    }
    match password {
        Some(password) if !password.is_empty() => {
            verify_password_async(password, &user.password_hash).await
        }
        _ => Err(ApiError::WrongPassword),
    }
}

async fn send_security_email(state: &AppState, user: &User, template: EmailTemplate) {
    let Some(email) = user.email.clone() else {
        return;
    };
    let locale = account::user_locale(state, user.id)
        .await
        .unwrap_or_else(|_| "en".into());
    if let Err(e) = enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(user.id),
            to: email,
            locale,
            template,
        },
    )
    .await
    {
        error!(user_id = %user.id, error = %e, "Could not queue a security e-mail");
    }
}

async fn public_user(state: &AppState, id: uuid::Uuid) -> Result<UserPublic, ApiError> {
    state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)
}

// ---------------------------------------------------------------------
// Security overview
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/users/me/security",
    tags = ["Account"],
    summary = "Security overview of the current account.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Security overview.", body = SecurityOverview))
)]
pub async fn get_security(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_account(&state, &access).await?;
    let public = public_user(&state, user.id).await?;
    let identities = state.security_repo.list_identities(user.id).await?;
    let remaining = if user.two_factor_enabled() {
        state.security_repo.count_recovery_codes(user.id).await?
    } else {
        0
    };
    Ok(Json(SecurityOverview {
        two_factor_enabled: user.two_factor_enabled(),
        two_factor_enabled_at: user.totp_enabled_at,
        recovery_codes_remaining: remaining,
        password_set: user.password_set,
        email_verified: user.email_verified(),
        has_google: identities.iter().any(|i| i.provider == "google"),
        last_login_at: public.last_login_at,
    }))
}

// ---------------------------------------------------------------------
// E-mail verification and change
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/users/me/email/verification",
    tags = ["Account"],
    summary = "Send an e-mail verification code.",
    description = "Sends a 6-digit code (valid 15 minutes) to the current address. At most one per minute and 10 per day (`TOO_MANY_ATTEMPTS`, `meta.retry_after_seconds`). `EMAIL_REQUIRED` without an address, `EMAIL_ALREADY_VERIFIED` when already verified.",
    security(("jwt_token" = [])),
    responses(
        (status = 202, description = "Code sent.", body = CodeSentResponse),
        (status = 409, description = "Already verified."),
        (status = 429, description = "Too many codes requested."),
    )
)]
pub async fn send_email_verification(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_account(&state, &access).await?;
    let Some(email) = user.email.clone() else {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::EMAIL_REQUIRED,
            "Add an e-mail address first.",
        ));
    };
    if user.email_verified_at.is_some() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_ALREADY_VERIFIED,
            "This e-mail address is already verified.",
        ));
    }
    let locale = account::user_locale(&state, user.id).await?;
    let username = user.username.clone();
    let sent = account::send_code(
        &state,
        &user,
        VerificationPurpose::EmailVerification,
        &email,
        &locale,
        ip.addr(),
        |code| EmailTemplate::EmailVerificationCode {
            username,
            code,
            expires_minutes: CODE_TTL_MINUTES,
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(sent)))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/email/verify",
    tags = ["Account"],
    summary = "Verify the e-mail address with a code.",
    description = "`INVALID_CODE` (`meta.attempts_left`, 5 attempts per code) or `CODE_EXPIRED`. On success the address is verified and a pending referral is rewarded.",
    request_body = EmailCodePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Verified.", body = UserPublic),
        (status = 400, description = "Invalid or expired code."),
    )
)]
pub async fn verify_email(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<EmailCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if user.email_verified_at.is_some() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_ALREADY_VERIFIED,
            "This e-mail address is already verified.",
        ));
    }
    let code = account::consume_code(
        &state,
        user.id,
        VerificationPurpose::EmailVerification,
        &payload.code,
    )
    .await?;

    if state
        .user_repo
        .mark_email_verified(user.id, &code.target_email)
        .await?
    {
        info!(user_id = %user.id, "E-mail verified");
        billing::qualify_referral(&state, user.id).await?;
        send_security_email(
            &state,
            &user,
            EmailTemplate::Welcome {
                username: user.username.clone(),
            },
        )
        .await;
    } else {
        // The address changed after the code was sent.
        return Err(account::invalid_code(0));
    }

    Ok(Json(public_user(&state, user.id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/email/change",
    tags = ["Account"],
    summary = "Start an e-mail change.",
    description = "Sends a 6-digit code to the *new* address; the change applies once it is confirmed. `password` is required when the account has one (`WRONG_PASSWORD`). `EMAIL_TAKEN` when the address is in use. Same resend limits as the verification code.",
    request_body = EmailChangePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 202, description = "Code sent to the new address.", body = CodeSentResponse),
        (status = 401, description = "Wrong password."),
        (status = 409, description = "Address already in use."),
    )
)]
pub async fn start_email_change(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<EmailChangePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    confirm_password(&user, payload.password.as_deref()).await?;

    let new_email = normalize_email(&payload.new_email);
    if user
        .email
        .as_deref()
        .is_some_and(|current| current.eq_ignore_ascii_case(&new_email))
    {
        return Err(ApiError::BadRequest(
            "This is already your e-mail address.".into(),
        ));
    }
    if state
        .user_repo
        .is_email_taken(&new_email, Some(user.id))
        .await?
    {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_TAKEN,
            "This e-mail address is already in use.",
        ));
    }

    let locale = account::user_locale(&state, user.id).await?;
    let username = user.username.clone();
    let sent = account::send_code(
        &state,
        &user,
        VerificationPurpose::EmailChange,
        &new_email,
        &locale,
        ip.addr(),
        |code| EmailTemplate::EmailChangeCode {
            username,
            code,
            expires_minutes: CODE_TTL_MINUTES,
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(sent)))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/email/change/confirm",
    tags = ["Account"],
    summary = "Confirm an e-mail change.",
    description = "Applies the new address (verified) and sends a notice to the previous one.",
    request_body = EmailCodePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "E-mail changed.", body = UserPublic),
        (status = 400, description = "Invalid or expired code."),
        (status = 409, description = "The address was taken meanwhile."),
    )
)]
pub async fn confirm_email_change(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<EmailCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    let code = account::consume_code(
        &state,
        user.id,
        VerificationPurpose::EmailChange,
        &payload.code,
    )
    .await?;
    if state
        .user_repo
        .is_email_taken(&code.target_email, Some(user.id))
        .await?
    {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_TAKEN,
            "This e-mail address is already in use.",
        ));
    }
    state
        .user_repo
        .change_email(user.id, &code.target_email)
        .await?;

    AuditEvent::by(&access, actions::USER_EMAIL_CHANGED)
        .target("user", user.id, &user.username)
        .meta(json!({ "from": user.email.as_deref().map(mask_email), "to": mask_email(&code.target_email) }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    // The previous address learns about the change (it may be the only
    // way the real owner finds out).
    send_security_email(
        &state,
        &user,
        EmailTemplate::EmailChangedNotice {
            username: user.username.clone(),
            new_email_masked: mask_email(&code.target_email),
        },
    )
    .await;
    notify(
        &state,
        Notification::security_alert(user.id, "email_changed"),
    )
    .await;
    if user.email_verified_at.is_none() {
        billing::qualify_referral(&state, user.id).await?;
    }

    Ok(Json(public_user(&state, user.id).await?))
}

// ---------------------------------------------------------------------
// Two-factor authentication
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/users/me/2fa/setup",
    tags = ["Account"],
    summary = "Start setting up two-factor authentication.",
    description = "Generates a new secret (valid for 15 minutes until confirmed with `/2fa/enable`). `password` is required when the account has one. `TWO_FACTOR_ALREADY_ENABLED` when already on.",
    request_body = PasswordConfirmationPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Secret generated.", body = TwoFactorSetupResponse),
        (status = 401, description = "Wrong password."),
        (status = 409, description = "Already enabled."),
    )
)]
pub async fn setup_two_factor(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<PasswordConfirmationPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if user.two_factor_enabled() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::TWO_FACTOR_ALREADY_ENABLED,
            "Two-factor authentication is already enabled.",
        ));
    }
    confirm_password(&user, payload.password.as_deref()).await?;

    let secret = random_bytes::<{ totp::SECRET_LEN }>();
    state
        .user_repo
        .set_pending_totp(user.id, &encrypt(&secret)?)
        .await?;
    let secret_b32 = totp::base32_encode(&secret);
    Ok(Json(TwoFactorSetupResponse {
        otpauth_url: totp::otpauth_url("Setlyst", &user.username, &secret_b32),
        secret: secret_b32,
        expires_at: Utc::now().naive_utc() + Duration::minutes(TOTP_SETUP_TTL_MINUTES),
    }))
}

async fn issue_recovery_codes(state: &AppState, user: &User) -> Result<Vec<String>, ApiError> {
    let codes: Vec<String> = (0..RECOVERY_CODE_COUNT).map(|_| recovery_code()).collect();
    let hashes: Vec<String> = codes.iter().map(|c| hash_code("recovery", c)).collect();
    state
        .security_repo
        .replace_recovery_codes(user.id, &hashes)
        .await?;
    Ok(codes)
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/2fa/enable",
    tags = ["Account"],
    summary = "Confirm and enable two-factor authentication.",
    description = "`code` is a current code from the authenticator app for the secret from `/2fa/setup` (`INVALID_TWO_FACTOR_CODE`; `CODE_EXPIRED` when the setup is older than 15 minutes or missing). Returns 10 recovery codes, shown only this once.",
    request_body = TwoFactorCodePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Enabled.", body = RecoveryCodesResponse),
        (status = 400, description = "Setup expired."),
        (status = 401, description = "Invalid code."),
        (status = 409, description = "Already enabled."),
    )
)]
pub async fn enable_two_factor(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<TwoFactorCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if user.two_factor_enabled() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::TWO_FACTOR_ALREADY_ENABLED,
            "Two-factor authentication is already enabled.",
        ));
    }
    let (Some(pending), Some(created_at)) =
        (&user.totp_pending_secret_enc, user.totp_pending_created_at)
    else {
        return Err(account::code_expired());
    };
    if created_at + Duration::minutes(TOTP_SETUP_TTL_MINUTES) < Utc::now().naive_utc() {
        return Err(account::code_expired());
    }
    let secret = account::decrypt_totp_secret(user.id, pending)?;
    let unix = Utc::now().timestamp().max(0) as u64;
    let Some(step) = totp::verify(&secret, &payload.code, unix, None) else {
        return Err(invalid_two_factor_code(None));
    };

    state.user_repo.enable_totp(user.id, step).await?;
    let codes = issue_recovery_codes(&state, &user).await?;

    AuditEvent::by(&access, actions::USER_TWO_FACTOR_ENABLED)
        .target("user", user.id, &user.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    send_security_email(
        &state,
        &user,
        EmailTemplate::TwoFactorEnabled {
            username: user.username.clone(),
        },
    )
    .await;
    notify(
        &state,
        Notification::security_alert(user.id, "two_factor_enabled"),
    )
    .await;
    info!(user_id = %user.id, "Two-factor authentication enabled");

    Ok(Json(RecoveryCodesResponse {
        recovery_codes: codes,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/sessions/revoke",
    tags = ["Account"],
    summary = "Sign out everywhere.",
    description = "Invalidates every token issued so far for the caller's account, the current one included, so every device (a lost phone, a shared computer) must sign in again. Answers `{ reauth_required: true }`.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Every session revoked.", body = ReauthRequiredResponse))
)]
pub async fn revoke_my_sessions(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
) -> Result<impl IntoResponse, ApiError> {
    let user = current_account(&state, &access).await?;
    state.user_repo.revoke_sessions(user.id).await?;

    AuditEvent::by(&access, actions::USER_SESSIONS_REVOKED)
        .target("user", user.id, &user.username)
        .meta(json!({ "self_service": true }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(user_id = %user.id, "Signed out everywhere");

    Ok(Json(ReauthRequiredResponse {
        reauth_required: true,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/2fa/disable",
    tags = ["Account"],
    summary = "Disable two-factor authentication.",
    description = "Requires the password (when the account has one) **and** a current app code or an unused recovery code. Wrong codes answer `INVALID_TWO_FACTOR_CODE` (`meta.attempts_left`); after 5 wrong codes in 15 minutes (shared with regenerating the recovery codes) `TOO_MANY_ATTEMPTS` (429, `meta.retry_after_seconds`).",
    request_body = TwoFactorDisablePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Disabled."),
        (status = 401, description = "Wrong password or code."),
        (status = 409, description = "Not enabled."),
        (status = 429, description = "Too many wrong codes."),
    )
)]
pub async fn disable_two_factor(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<TwoFactorDisablePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if !user.two_factor_enabled() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::TWO_FACTOR_NOT_ENABLED,
            "Two-factor authentication is not enabled.",
        ));
    }
    confirm_password(&user, payload.password.as_deref()).await?;
    account::require_second_factor(&state, &user, &payload.code, "disable", &ip.0).await?;

    state.user_repo.disable_totp(user.id).await?;
    state.security_repo.delete_recovery_codes(user.id).await?;

    AuditEvent::by(&access, actions::USER_TWO_FACTOR_DISABLED)
        .target("user", user.id, &user.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    send_security_email(
        &state,
        &user,
        EmailTemplate::TwoFactorDisabled {
            username: user.username.clone(),
        },
    )
    .await;
    notify(
        &state,
        Notification::security_alert(user.id, "two_factor_disabled"),
    )
    .await;
    info!(user_id = %user.id, "Two-factor authentication disabled");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/users/me/2fa/recovery-codes",
    tags = ["Account"],
    summary = "Regenerate the recovery codes.",
    description = "Requires a current app code (or an unused recovery code). Every previous recovery code stops working. At most 5 wrong codes in 15 minutes (shared with disabling 2FA), then `TOO_MANY_ATTEMPTS` (429, `meta.retry_after_seconds`).",
    request_body = TwoFactorCodePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "New codes.", body = RecoveryCodesResponse),
        (status = 401, description = "Invalid code."),
        (status = 409, description = "Not enabled."),
        (status = 429, description = "Too many wrong codes."),
    )
)]
pub async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<TwoFactorCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if !user.two_factor_enabled() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::TWO_FACTOR_NOT_ENABLED,
            "Two-factor authentication is not enabled.",
        ));
    }
    account::require_second_factor(&state, &user, &payload.code, "recovery_codes", &ip.0).await?;
    let codes = issue_recovery_codes(&state, &user).await?;
    notify(
        &state,
        Notification::security_alert(user.id, "recovery_codes_regenerated"),
    )
    .await;
    Ok(Json(RecoveryCodesResponse {
        recovery_codes: codes,
    }))
}

// ---------------------------------------------------------------------
// Linked identities
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/users/me/identities",
    tags = ["Account"],
    summary = "Sign-in providers linked to the account.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Linked identities.", body = [LinkedIdentity]))
)]
pub async fn list_identities(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .security_repo
            .list_identities(access.user_id())
            .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/me/identities/{provider}",
    tags = ["Account"],
    summary = "Unlink a sign-in provider.",
    description = "Only `google` exists. Refused with `PASSWORD_NOT_SET` when the account has no password (it would have no way to sign in; set one through password recovery first).",
    params(("provider" = String, Path, description = "`google`")),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Unlinked."),
        (status = 404, description = "Not linked."),
        (status = 409, description = "The account has no password."),
    )
)]
pub async fn unlink_identity(
    State(state): State<AppState>,
    access: AccessControl,
    Path(provider): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if provider != "google" {
        return Err(ApiError::NotFound);
    }
    let user = current_account(&state, &access).await?;
    if !user.password_set {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::PASSWORD_NOT_SET,
            "Set a password (through password recovery) before unlinking Google.",
        ));
    }
    if !state
        .security_repo
        .unlink_identity(user.id, &provider)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Communication preferences
// ---------------------------------------------------------------------

async fn communication_settings(
    state: &AppState,
    user_id: uuid::Uuid,
) -> Result<CommunicationSettings, ApiError> {
    let (prefs, _) = state.user_prefs_repo.get_communication(user_id).await?;
    let user = public_user(state, user_id).await?;
    Ok(CommunicationSettings {
        categories: prefs.to_view(),
        email_verified: user.email_verified,
        email: user.email,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/communication",
    tags = ["Account"],
    summary = "Communication preferences.",
    description = "Per category: whether messages arrive by e-mail and in the app. `security` is locked on. E-mails only go to a verified address.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Preferences.", body = CommunicationSettings))
)]
pub async fn get_communication(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        communication_settings(&state, access.user_id()).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/api/v1/users/me/communication",
    tags = ["Account"],
    summary = "Update communication preferences.",
    description = "Only the categories sent change. Unknown categories are a `VALIDATION_ERROR`; `security` is ignored (always on).",
    request_body = UpdateCommunicationPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated preferences.", body = CommunicationSettings))
)]
pub async fn update_communication(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<UpdateCommunicationPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if payload.categories.len() > Category::ALL.len() {
        return Err(ApiError::BadRequest("Too many categories.".into()));
    }
    let (mut prefs, _) = state
        .user_prefs_repo
        .get_communication(access.user_id())
        .await?;
    for (key, value) in &payload.categories {
        let Some(category) = Category::from_key(key) else {
            let mut errors = validator::ValidationErrors::new();
            let mut error = validator::ValidationError::new("unknown_category");
            error.message = Some(format!("Unknown category '{key}'.").into());
            errors.add("categories", error);
            return Err(ApiError::ValidationError(errors));
        };
        prefs.set(category, *value);
    }
    state
        .user_prefs_repo
        .set_communication(access.user_id(), &prefs)
        .await?;
    Ok(Json(
        communication_settings(&state, access.user_id()).await?,
    ))
}

// ---------------------------------------------------------------------
// Consent and deletion
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/users/me/accept-terms",
    tags = ["Account"],
    summary = "Accept the current Terms of Use and Privacy Policy.",
    description = "`version` must be the version in force (`GET /public/legal/version`), else `BAD_REQUEST`.",
    request_body = AcceptTermsPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Accepted.", body = UserPublic))
)]
pub async fn accept_terms(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<AcceptTermsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    if payload.version.trim() != CURRENT_TERMS_VERSION {
        return Err(ApiError::BadRequest(format!(
            "The terms in force are version {CURRENT_TERMS_VERSION}."
        )));
    }
    state
        .user_repo
        .accept_terms(access.user_id(), CURRENT_TERMS_VERSION)
        .await?;
    AuditEvent::by(&access, actions::USER_TERMS_ACCEPTED)
        .target("user", access.user_id(), &access.0.username)
        .meta(json!({ "version": CURRENT_TERMS_VERSION }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(public_user(&state, access.user_id()).await?))
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/me",
    tags = ["Account"],
    summary = "Delete the current account.",
    description = "Self-service deletion (LGPD art. 18, VI). Requires `confirmation` equal to the username and the password when the account has one. Bands the user owned are handed to their most senior remaining member. A goodbye e-mail is sent. The last active admin can't delete their account (`LAST_ADMIN`).",
    request_body = DeleteAccountPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Account deleted."),
        (status = 400, description = "Confirmation does not match."),
        (status = 401, description = "Wrong password."),
        (status = 409, description = "Last admin."),
    )
)]
pub async fn delete_current_user(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<DeleteAccountPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = current_account(&state, &access).await?;
    if payload.confirmation.trim() != user.username {
        return Err(ApiError::BadRequest(
            "Type your username exactly to confirm.".into(),
        ));
    }
    confirm_password(&user, payload.password.as_deref()).await?;

    // Queued before the deletion, with no user reference (which would
    // cascade away), so the goodbye reaches the owner. Withdrawn if the
    // deletion fails.
    let locale = account::user_locale(&state, user.id).await?;
    let goodbye = match user.email.clone() {
        Some(email) => Some(
            enqueue(
                &state.db,
                &OutgoingEmail {
                    user_id: None,
                    to: email,
                    locale,
                    template: EmailTemplate::AccountDeleted {
                        username: user.username.clone(),
                    },
                },
            )
            .await?,
        ),
        None => None,
    };

    if let Err(e) = state.user_repo.delete(user.id).await {
        if let Some(id) = goodbye {
            sqlx::query("DELETE FROM email_outbox WHERE id = $1 AND status = 'pending'")
                .bind(id)
                .execute(&state.db)
                .await?;
        }
        return Err(e);
    }

    // The actor row is gone: only its name is kept (the actor id column
    // references `users`).
    let mut event = AuditEvent::new(actions::USER_SELF_DELETED);
    event.actor_username = Some(user.username.clone());
    event
        .target("user", user.id, &user.username)
        .meta(json!({ "role": user.role }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(user_id = %user.id, "Account deleted by its owner");
    Ok(StatusCode::NO_CONTENT)
}
