use crate::{
    database::{
        AppState,
        repositories::audit_repository::{AuditEvent, record_legal_acceptances},
    },
    email::{EmailTemplate, OutgoingEmail, enqueue, outbox::mask_email},
    errors::api_error::{ApiError, codes},
    middlewares::authentication::check_claims,
    models::{
        audit::actions,
        auth::{
            ForgotPasswordPayload, GoogleSignInPayload, LoginOutcome, LoginPayload, LoginResponse,
            LoginTwoFactorPayload, ReauthRequiredResponse, ResetPasswordPayload,
            TwoFactorChallengeResponse, access::ClientIp, token::VerifyTokenPayload,
        },
        notification::Notification,
        security::{VerificationCode, VerificationPurpose},
        user::{CURRENT_TERMS_VERSION, RegisterPayload, Status, User, UserPublic},
    },
    services::{
        account::{
            self, AccountDraft, CHALLENGE_MAX_ATTEMPTS, CODE_TTL_MINUTES, LOCKOUT_THRESHOLD,
            account_locked, code_expired, invalid_code_plain, invalid_two_factor_code,
        },
        billing,
        google::{GoogleIdentity, GoogleVerifyError},
        notifier::notify,
    },
    utils::{
        crypto::{random_token, sha256_hex},
        hashing::{dummy_verify_async, hash_password, verify_password_upgrading_async},
        jwt::decode_jwt,
    },
    validations::password::{is_password_compliant, password_issues_checked},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::json;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use validator::Validate;

/// Password-recovery requests take at least this long, whether or not the
/// account exists, so timing doesn't reveal which identifiers are
/// registered.
const FORGOT_MIN_DURATION: Duration = Duration::from_millis(250);

/// Refuses deactivated and suspended accounts, with the same errors as a
/// password sign-in.
pub(crate) fn ensure_can_sign_in(user: &User) -> Result<(), ApiError> {
    if user.status != Status::Active {
        return Err(ApiError::account_deactivated());
    }
    if let Some(until) = user.active_ban() {
        return Err(ApiError::account_banned(until, user.ban_reason.clone()));
    }
    Ok(())
}

/// Returns a JWT if the credentials passed are valid.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tags = ["Auth"],
    summary = "Sign in and receive a JWT.",
    description = "`username` accepts the username or the e-mail address (case-insensitive). Unknown accounts, wrong passwords and networks locked out of the account are indistinguishable (`INVALID_CREDENTIALS`). After 5 consecutive failures from one network (IPv4 address or IPv6 /64) that network is locked out of the account for 15 minutes (doubling with each further lock, up to 24 hours), without affecting sign-ins from elsewhere; 20 failures from anywhere within 15 minutes (or 5 wrong second factors) lock the account itself, which is only revealed after a correct password: `ACCOUNT_LOCKED` (429, `meta.until`). The owner is e-mailed when a lock happens; password recovery lifts every lock. Suspended or deactivated accounts are only reported after a correct password (`ACCOUNT_BANNED` / `ACCOUNT_DEACTIVATED`).\n\nWhen the account has two-factor authentication, the answer is `{ two_factor_required: true, challenge_token, challenge_expires_at }` and the sign-in continues at `POST /auth/login/2fa`. Otherwise it is a `LoginResponse` with `two_factor_required: false`. `SERVICE_BUSY` (503) when password checks are saturated.",
    request_body = LoginPayload,
    responses(
        (status = 200, description = "Signed in, or second factor required.", body = LoginOutcome),
        (status = 401, description = "Invalid credentials."),
        (status = 403, description = "Account suspended or deactivated."),
        (status = 429, description = "Account locked."),
        (status = 503, description = "Service busy."),
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

    let Some(user) = state
        .user_repo
        .find_by_identifier(&payload.username)
        .await?
    else {
        // Spend the same time as a real check so the response can't be
        // used to probe which accounts exist; the audit write happens in
        // the background for the same reason. The typed identifier is
        // only kept masked (it may be someone's e-mail, or a password
        // typed into the wrong field).
        dummy_verify_async(&payload.password).await?;
        AuditEvent::new(actions::USER_LOGIN_FAILED)
            .target_label("user", &account::mask_identifier(&payload.username))
            .meta(json!({ "reason": "unknown_account" }))
            .ip(&ip.0)
            .spawn(state.audit_repo.clone());
        return Err(ApiError::invalid_credentials());
    };

    // A network locked out of the account gets the answer of a wrong
    // password without the password being looked at: the lock stops the
    // guessing, and the answer tells nothing (not even that the account
    // exists).
    let bucket = account::ip_bucket(ip.addr());
    if state
        .user_repo
        .network_locked_until(user.id, &bucket)
        .await?
        .is_some()
    {
        dummy_verify_async(&payload.password).await?;
        return Err(ApiError::invalid_credentials());
    }

    match verify_password_upgrading_async(&payload.password, &user.password_hash).await {
        Ok(false) => {}
        Ok(true) => {
            // Stored before passwords were normalized: store the
            // normalized form now that the password is known.
            match hash_password(&payload.password).await {
                Ok(hash) => {
                    if let Err(e) = state
                        .user_repo
                        .upgrade_password_hash(user.id, &user.password_hash, &hash)
                        .await
                    {
                        warn!(user_id = %user.id, error = %e, "Could not upgrade a legacy password hash");
                    }
                }
                Err(e) => {
                    warn!(user_id = %user.id, error = %e, "Could not rehash a legacy password")
                }
            }
        }
        Err(ApiError::WrongPassword) => {
            // Recorded in the background, like the unknown-account audit,
            // so both answers take the same time.
            spawn_password_failure(state.clone(), user, bucket, ip.0.clone());
            return Err(ApiError::invalid_credentials());
        }
        Err(e) => return Err(e),
    }

    // The account-wide lock (and the account state) is only revealed to
    // someone who proved they know the password.
    if let Some(until) = user.locked_until_active() {
        return Err(account_locked(until));
    }
    ensure_can_sign_in(&user)?;

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

    if user.two_factor_enabled() {
        // The failure counter is NOT reset here: the password alone isn't a
        // sign-in. Resetting it would let someone who knows the password
        // start over after every few wrong 2FA codes (unlimited guessing);
        // only `issue_session` (a complete sign-in) clears it. The new
        // challenge also invalidates any older one.
        let challenge =
            account::create_challenge(&state, &user, "password", ip.addr(), None).await?;
        info!(user_id = %user.id, "Password accepted; second factor required");
        return Ok(Json(LoginOutcome::TwoFactor(challenge)));
    }

    let response = account::issue_session(
        &state,
        &user,
        must_change_password,
        false,
        "password",
        &ip.0,
    )
    .await?;
    info!(user_id = %user.id, "Login successful");
    Ok(Json(LoginOutcome::Session(response)))
}

/// Records a wrong password from `bucket` in the background: audit, and
/// when it locks the network (or the whole account) out, the lock's audit
/// entry and the owner's notice.
fn spawn_password_failure(state: AppState, user: User, bucket: String, ip: Option<String>) {
    tokio::spawn(async move {
        let failure = match state
            .user_repo
            .record_password_failure(user.id, &bucket)
            .await
        {
            Ok(failure) => failure,
            Err(e) => {
                error!(user_id = %user.id, error = %e, "Could not record a failed sign-in");
                return;
            }
        };
        AuditEvent::new(actions::USER_LOGIN_FAILED)
            .target("user", user.id, &user.username)
            .meta(json!({ "consecutive_failures": failure.network_failures }))
            .ip(&ip)
            .record(&*state.audit_repo)
            .await;
        let lock = failure.account_lock.or(failure.network_lock);
        if let Some(until) = lock {
            warn!(user_id = %user.id, %until, account_wide = failure.account_lock.is_some(), "Sign-ins locked after repeated failures");
            AuditEvent::new(actions::USER_LOGIN_LOCKED)
                .target("user", user.id, &user.username)
                .meta(json!({
                    "until": until,
                    "consecutive_failures": failure.network_failures,
                    "scope": if failure.account_lock.is_some() { "account" } else { "network" },
                }))
                .ip(&ip)
                .record(&*state.audit_repo)
                .await;
            account::notify_login_locked(&state, &user).await;
        }
    });
}

/// Links `identity` (Google) to `user`, recording it and telling the
/// owner (audit, security e-mail, notification).
pub(crate) async fn link_google_identity(
    state: &AppState,
    user: &User,
    subject: &str,
    email: &str,
    ip: &Option<String>,
    source: &str,
) -> Result<(), ApiError> {
    state
        .security_repo
        .link_identity(user.id, "google", subject, Some(email))
        .await?;
    AuditEvent::new(actions::USER_IDENTITY_LINKED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .meta(json!({ "provider": "google", "email": mask_email(email), "source": source }))
        .ip(ip)
        .record(&*state.audit_repo)
        .await;
    account::send_security_notice(state, user, "google_linked", Some(mask_email(email)), None)
        .await;
    notify(
        state,
        Notification::security_alert(user.id, "google_linked"),
    )
    .await;
    info!(user_id = %user.id, %source, "Google identity linked");
    Ok(())
}

/// Completes a sign-in with the second factor.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login/2fa",
    tags = ["Auth"],
    summary = "Complete a sign-in with a two-factor code.",
    description = "Send the `challenge_token` from the first step with either `code` (6 digits from the authenticator app) or `recovery_code` (`XXXX-XXXX`, single use). A challenge lasts 5 minutes, allows 5 attempts and works once. Wrong codes answer `INVALID_TWO_FACTOR_CODE` (401, `meta.attempts_left`); an expired challenge answers `CODE_EXPIRED`. Wrong codes also count towards the account lockout.",
    request_body = LoginTwoFactorPayload,
    responses(
        (status = 200, description = "Signed in.", body = LoginResponse),
        (status = 400, description = "Challenge expired or malformed request."),
        (status = 401, description = "Invalid code or challenge."),
        (status = 429, description = "Account locked."),
    )
)]
pub async fn login_two_factor(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(payload): Json<LoginTwoFactorPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let challenge = state
        .security_repo
        .find_challenge(&sha256_hex(payload.challenge_token.trim().as_bytes()))
        .await?
        .ok_or_else(|| invalid_two_factor_code(None))?;

    if challenge.consumed_at.is_some() {
        return Err(invalid_two_factor_code(Some(0)));
    }
    if challenge.expires_at <= chrono::Utc::now().naive_utc() {
        return Err(code_expired());
    }

    let user = state
        .user_repo
        .find_account(challenge.user_id)
        .await?
        .ok_or_else(|| invalid_two_factor_code(None))?;
    // A challenge minted before the latest password change was earned
    // with a password that no longer works.
    if user
        .password_changed_at
        .is_some_and(|changed| challenge.created_at < changed)
    {
        return Err(invalid_two_factor_code(Some(0)));
    }
    if let Some(until) = user.locked_until_active() {
        return Err(account_locked(until));
    }
    ensure_can_sign_in(&user)?;

    let (code, recovery) = match (&payload.code, &payload.recovery_code) {
        (Some(code), None) => (Some(code), None),
        (None, Some(recovery)) => (None, Some(recovery)),
        _ => {
            return Err(ApiError::BadRequest(
                "Send either 'code' or 'recovery_code'.".into(),
            ));
        }
    };

    // The attempt is claimed before the code is checked, in the same
    // statement as the limit: concurrent guesses can't slip past the 5
    // attempts of a challenge.
    let Some(attempts) = state
        .security_repo
        .claim_challenge_attempt(challenge.id, CHALLENGE_MAX_ATTEMPTS)
        .await?
    else {
        return Err(if challenge.expires_at <= chrono::Utc::now().naive_utc() {
            code_expired()
        } else {
            invalid_two_factor_code(Some(0))
        });
    };

    let accepted = match (code, recovery) {
        (Some(code), _) => account::check_totp(&state, &user, code).await?,
        (_, Some(recovery)) => account::use_recovery_code(&state, &user, recovery).await?,
        (None, None) => false,
    };

    if !accepted {
        if attempts >= CHALLENGE_MAX_ATTEMPTS {
            state.security_repo.consume_challenge(challenge.id).await?;
        }
        let (failures, lock) = state
            .user_repo
            .record_failed_login(user.id, LOCKOUT_THRESHOLD)
            .await?;
        AuditEvent::new(actions::USER_LOGIN_FAILED)
            .target("user", user.id, &user.username)
            .meta(json!({ "second_factor": true, "consecutive_failures": failures }))
            .ip(&ip.0)
            .spawn(state.audit_repo.clone());
        if let Some(until) = lock {
            warn!(user_id = %user.id, %until, "Account locked after repeated wrong second factors");
            AuditEvent::new(actions::USER_LOGIN_LOCKED)
                .target("user", user.id, &user.username)
                .meta(json!({ "until": until, "consecutive_failures": failures, "second_factor": true }))
                .ip(&ip.0)
                .spawn(state.audit_repo.clone());
            account::notify_login_locked(&state, &user).await;
            return Err(account_locked(until));
        }
        return Err(invalid_two_factor_code(Some(
            CHALLENGE_MAX_ATTEMPTS - attempts,
        )));
    }

    // Single use, even against a concurrent correct attempt.
    if !state.security_repo.consume_challenge(challenge.id).await? {
        return Err(invalid_two_factor_code(Some(0)));
    }

    if payload.recovery_code.is_some() {
        let remaining = state.security_repo.count_recovery_codes(user.id).await?;
        info!(user_id = %user.id, remaining, "Signed in with a recovery code");
    }

    // A Google sign-in that matched this account by e-mail links the
    // identity only now, after the second factor.
    if let (Some(subject), Some(email)) = (
        &challenge.pending_link_subject,
        &challenge.pending_link_email,
    ) && state
        .security_repo
        .find_identity("google", subject)
        .await?
        .is_none()
        && !state
            .security_repo
            .list_identities(user.id)
            .await?
            .iter()
            .any(|i| i.provider == "google")
    {
        link_google_identity(&state, &user, subject, email, &ip.0, "google_sign_in").await?;
    }

    let method = format!("{}+two_factor", challenge.method);
    let response = account::issue_session(
        &state,
        &user,
        user.must_change_password,
        false,
        &method,
        &ip.0,
    )
    .await?;
    info!(user_id = %user.id, method = %challenge.method, "Login successful (two-factor)");
    Ok(Json(response))
}

/// Register a new user.
#[utoipa::path(
    post,
    path = "/api/v1/auth/register",
    tags = ["Auth"],
    summary = "Register a new user.",
    description = "Creates a regular account. `email` is required (`EMAIL_REQUIRED`) and must be unused (`EMAIL_TAKEN`); `accept_terms` must be `true` (`TERMS_NOT_ACCEPTED`) and so must `age_confirmed` (`AGE_CONFIRMATION_REQUIRED`). The password must satisfy the platform policy (`WEAK_PASSWORD` lists the failed rules in `meta.issues`; `breached` when it appears in a public breach corpus). A 6-digit verification code is e-mailed right away. With `referral_code`, the referrer is credited once this account verifies its e-mail. When plans are enforced, the trial starts once the e-mail is verified (one trial per address). Rate-limited per IP (5 per hour).",
    request_body = RegisterPayload,
    responses(
        (status = 201, description = "User registered successfully.", body = UserPublic),
        (status = 400, description = "Invalid input, weak password, missing e-mail or terms not accepted."),
        (status = 409, description = "Username or e-mail already taken.")
    )
)]
pub async fn register(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Json(payload): Json<RegisterPayload>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received registration request");

    let issues = password_issues_checked(&payload.password, Some(&payload.username)).await;
    if !issues.is_empty() {
        return Err(ApiError::weak_password(&issues));
    }

    let Some(email) = payload
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(str::to_string)
    else {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::EMAIL_REQUIRED,
            "An e-mail address is required.",
        ));
    };

    payload.validate()?;

    if !payload.accept_terms {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::TERMS_NOT_ACCEPTED,
            "You must accept the Terms of Use and the Privacy Policy.",
        ));
    }
    if !payload.age_confirmed {
        return Err(ApiError::age_confirmation_required());
    }

    state.user_repo.is_unique(&payload.username, None).await?;
    if state.user_repo.is_email_taken(&email, None).await? {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::EMAIL_TAKEN,
            "This e-mail address is already in use.",
        ));
    }

    let locale = account::supported_locale(payload.locale.as_deref());
    let password_hash = hash_password(&payload.password).await?;
    let new_user = account::create_account(
        &state,
        AccountDraft {
            username: payload.username.trim().to_string(),
            email,
            password_hash,
            password_set: true,
            email_verified: false,
            first_name: crate::models::user::clearable(&payload.first_name).flatten(),
            last_name: crate::models::user::clearable(&payload.last_name).flatten(),
            terms_version: Some(CURRENT_TERMS_VERSION.to_string()),
            referral_code: payload.referral_code.clone(),
            marketing_opt_in: payload.marketing_opt_in,
            locale: locale.clone(),
            age_attested: true,
        },
    )
    .await?;

    AuditEvent::new(actions::USER_REGISTERED)
        .actor(new_user.id, &new_user.username)
        .target("user", new_user.id, &new_user.username)
        .meta(json!({
            "referred": payload.referral_code.is_some(),
            "terms_version": CURRENT_TERMS_VERSION,
            "marketing_opt_in": payload.marketing_opt_in,
            "age_confirmed": true,
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    record_legal_acceptances(
        &*state.audit_repo,
        &account::signup_acceptances(
            new_user.id,
            "register",
            payload.marketing_opt_in,
            &ip.0,
            account::user_agent(&headers),
        ),
    )
    .await;

    if let Some(email) = &new_user.email {
        let username = new_user.username.clone();
        if let Err(e) = account::send_code(
            &state,
            &new_user,
            VerificationPurpose::EmailVerification,
            email,
            &locale,
            ip.addr(),
            |code| EmailTemplate::EmailVerificationCode {
                username,
                code,
                expires_minutes: CODE_TTL_MINUTES,
            },
        )
        .await
        {
            error!(user_id = %new_user.id, error = %e, "Could not send the verification code");
        }
    }

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
    description = "Checks the signature, the expiry *and* that the token hasn't been revoked (password change, suspension, deactivation, sign-out everywhere). Impersonation tokens are checked like on every request: the impersonator must still be active, outrank the target and not have been signed out since.",
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

    if let Err(e) = check_claims(&state, &claims, None).await {
        warn!(user_id = %claims.sub, "Rejected a revoked token at /auth/verify");
        return Err(match e {
            ApiError::DatabaseError(_) => e,
            _ => ApiError::session_revoked(),
        });
    }

    Ok((StatusCode::OK, Json(json!({ "valid": true }))))
}

/// Starts a password recovery.
#[utoipa::path(
    post,
    path = "/api/v1/auth/password/forgot",
    tags = ["Auth"],
    summary = "Request a password recovery code.",
    description = "Always answers `202 {}`, whether or not the account exists (no enumeration). When `identifier` (username or e-mail) matches an active account with an e-mail address, a 6-digit code valid for 15 minutes is sent to that address (at most one per minute). Rate-limited per IP.",
    request_body = ForgotPasswordPayload,
    responses((status = 202, description = "Accepted."))
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(payload): Json<ForgotPasswordPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let started = Instant::now();

    if let Some(user) = state
        .user_repo
        .find_by_identifier(&payload.identifier)
        .await?
        && user.status == Status::Active
        && let Some(email) = user.email.clone()
    {
        let locale = account::user_locale(&state, user.id).await?;
        let username = user.username.clone();
        match account::send_code(
            &state,
            &user,
            VerificationPurpose::PasswordReset,
            &email,
            &locale,
            ip.addr(),
            |code| EmailTemplate::PasswordResetCode {
                username,
                code,
                expires_minutes: CODE_TTL_MINUTES,
            },
        )
        .await
        {
            Ok(_) => info!(user_id = %user.id, "Password recovery code sent"),
            // Throttled: silently, so the answer stays the same.
            Err(ApiError::Rule { code, .. }) if code == codes::TOO_MANY_ATTEMPTS => {}
            Err(e) => error!(user_id = %user.id, error = %e, "Could not send a recovery code"),
        }
    }

    if let Some(remaining) = FORGOT_MIN_DURATION.checked_sub(started.elapsed()) {
        tokio::time::sleep(remaining).await;
    }
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// Sets a new password with a recovery code.
#[utoipa::path(
    post,
    path = "/api/v1/auth/password/reset",
    tags = ["Auth"],
    summary = "Set a new password with a recovery code.",
    description = "Checks the code first (`INVALID_CODE`, without any count of the attempts left; `CODE_EXPIRED`; 5 attempts per code and 10 wrong codes per account per day, then `TOO_MANY_ATTEMPTS`; an unknown identifier answers like a wrong code, in the same time), then the password policy (`WEAK_PASSWORD`, which leaves the code usable for another try), then sets the password, signs the account out everywhere, clears every lockout and marks the e-mail as verified. When that is the first proof that the owner controls the address, two-factor authentication and linked sign-in providers set up before are removed. Rate-limited per IP.",
    request_body = ResetPasswordPayload,
    responses(
        (status = 200, description = "Password changed; sign in again.", body = ReauthRequiredResponse),
        (status = 400, description = "Invalid or expired code, or weak password."),
    )
)]
pub async fn reset_password(
    State(state): State<AppState>,
    ip: ClientIp,
    Json(payload): Json<ResetPasswordPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let started = Instant::now();
    let checked = check_reset_code(&state, &payload, &ip).await;
    // Unknown accounts and wrong codes take the same time and give the
    // same answer, so the endpoint can't be used to find accounts.
    if checked.is_err()
        && let Some(remaining) = FORGOT_MIN_DURATION.checked_sub(started.elapsed())
    {
        tokio::time::sleep(remaining).await;
    }
    let (user, code) = checked?;

    // The password policy is only checked after the code: before, a
    // `WEAK_PASSWORD` would confirm the identifier exists. A weak password
    // gives the attempt back, so the owner can simply try another one.
    let issues = password_issues_checked(&payload.new_password, Some(&user.username)).await;
    if !issues.is_empty() {
        state.security_repo.release_code_attempt(code.id).await?;
        return Err(ApiError::weak_password(&issues));
    }
    if !state.security_repo.consume_code(code.id).await? {
        return Err(invalid_code_plain());
    }

    let verifies_email = user
        .email
        .as_deref()
        .is_some_and(|e| e.eq_ignore_ascii_case(&code.target_email));
    let first_proof = state
        .user_repo
        .recover_password(user.id, &payload.new_password, verifies_email)
        .await?;

    AuditEvent::new(actions::USER_PASSWORD_RECOVERED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    if first_proof {
        AuditEvent::new(actions::USER_SECURITY_RESET)
            .actor(user.id, &user.username)
            .target("user", user.id, &user.username)
            .meta(json!({ "reason": "first_email_proof", "had_two_factor": user.two_factor_enabled() }))
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }

    if let Some(email) = &user.email {
        let locale = account::user_locale(&state, user.id).await?;
        if let Err(e) = enqueue(
            &state.db,
            &OutgoingEmail {
                user_id: Some(user.id),
                to: email.clone(),
                locale,
                template: EmailTemplate::PasswordChanged {
                    username: user.username.clone(),
                },
            },
        )
        .await
        {
            error!(user_id = %user.id, error = %e, "Could not queue the password notice");
        }
    }
    notify(
        &state,
        Notification::security_alert(user.id, "password_changed"),
    )
    .await;

    if verifies_email && user.email_verified_at.is_none() {
        billing::qualify_referral(&state, user.id).await?;
        account::start_verified_trial(&state, user.id, &code.target_email).await?;
    }

    info!(user_id = %user.id, "Password recovered; sessions revoked");
    Ok(Json(ReauthRequiredResponse {
        reauth_required: true,
    }))
}

/// The account and its (still unconsumed) recovery code. An unknown or
/// inactive account answers exactly like an account without a live code,
/// and no answer says how many attempts are left (that count would tell
/// which identifiers have an account).
async fn check_reset_code(
    state: &AppState,
    payload: &ResetPasswordPayload,
    ip: &ClientIp,
) -> Result<(User, VerificationCode), ApiError> {
    let Some(user) = state
        .user_repo
        .find_by_identifier(&payload.identifier)
        .await?
        .filter(|u| u.status == Status::Active)
    else {
        return Err(invalid_code_plain());
    };
    match account::check_code(
        state,
        user.id,
        VerificationPurpose::PasswordReset,
        &payload.code,
    )
    .await
    {
        Ok(code) => Ok((user, code)),
        Err(e) => {
            AuditEvent::new(actions::USER_PASSWORD_RESET_FAILED)
                .target("user", user.id, &user.username)
                .meta(json!({ "reason": e.code() }))
                .ip(&ip.0)
                .spawn(state.audit_repo.clone());
            Err(match e.code() {
                codes::INVALID_CODE => invalid_code_plain(),
                _ => e,
            })
        }
    }
}

/// Signs in (or up) with a Google ID token.
#[utoipa::path(
    post,
    path = "/api/v1/auth/oauth/google",
    tags = ["Auth"],
    summary = "Sign in with Google.",
    description = "Verifies the Google ID token (`INVALID_GOOGLE_TOKEN`; `GOOGLE_SIGNIN_DISABLED` when not configured). The account is found by its linked Google identity, else by a *verified* e-mail address, which links it only when Google is authoritative for the address (Gmail, or a Workspace account of the address's domain); for any other address the answer is `ACCOUNT_LINK_REQUIRED` (409): sign in with the password and link Google from the settings (`POST /users/me/identities/google`). The link is made only after the account's state and second factor checks pass, and the owner is notified. An *unverified* account with the same address loses the address (Google proved who owns it) and a new account is created. A new account requires `accept_terms: true` (else `TERMS_NOT_ACCEPTED` with `meta: { signup: true, email, name }`, so the client can ask for consent and retry) and `age_confirmed: true` (`AGE_CONFIRMATION_REQUIRED`). Suspended or deactivated accounts get the same errors as a password sign-in; accounts with two-factor authentication get the same challenge. The password lockout doesn't apply. Rate-limited per IP.",
    request_body = GoogleSignInPayload,
    responses(
        (status = 200, description = "Signed in, or second factor required.", body = LoginOutcome),
        (status = 400, description = "Consent to the terms or the age declaration required for a new account."),
        (status = 401, description = "Invalid token."),
        (status = 409, description = "The matching account must link Google from its settings."),
        (status = 503, description = "Google sign-in disabled."),
    )
)]
pub async fn google_sign_in(
    State(state): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    Json(payload): Json<GoogleSignInPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let disabled = || {
        ApiError::rule(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::GOOGLE_SIGNIN_DISABLED,
            "Sign-in with Google is not available.",
        )
    };
    if !state.google_verifier.enabled() {
        return Err(disabled());
    }
    payload.validate()?;

    let identity = match state.google_verifier.verify(payload.id_token.trim()).await {
        Ok(identity) => identity,
        Err(GoogleVerifyError::Disabled) => return Err(disabled()),
        Err(GoogleVerifyError::Invalid(reason)) => {
            debug!(%reason, "Google token rejected");
            return Err(ApiError::rule(
                StatusCode::UNAUTHORIZED,
                codes::INVALID_GOOGLE_TOKEN,
                "The Google sign-in could not be verified. Please try again.",
            ));
        }
    };

    // 1. An account this Google identity is already linked to.
    if let Some(user_id) = state
        .security_repo
        .find_identity("google", &identity.subject)
        .await?
    {
        let user = state
            .user_repo
            .find_account(user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        state
            .security_repo
            .touch_identity("google", &identity.subject)
            .await?;
        return finish_google_sign_in(&state, user, None, false, &ip).await;
    }

    // 2. An account with the same address.
    if let Some(existing) = state.user_repo.find_by_email(&identity.email).await? {
        if existing.email_verified_at.is_some() {
            // Only an address Google is authoritative for may link an
            // account silently; for any other, the owner signs in with
            // the password and links Google from the settings.
            if !identity.is_authoritative_for_email() {
                info!(user_id = %existing.id, "Google sign-in matched an account it can't vouch for");
                return Err(ApiError::account_link_required());
            }
            // Another Google account is linked already: never replaced
            // silently.
            if state
                .security_repo
                .list_identities(existing.id)
                .await?
                .iter()
                .any(|i| i.provider == "google")
            {
                return Err(ApiError::account_link_required());
            }
            return finish_google_sign_in(&state, existing, Some(&identity), false, &ip).await;
        }
        // The address was claimed without ever being proven. Google just
        // proved who owns it: it is taken away from the unverified account
        // (which keeps everything else and can add another address), and
        // the sign-up below proceeds.
        if !payload.accept_terms || !payload.age_confirmed {
            return Err(signup_consent_error(&payload, &identity));
        }
        if state.user_repo.detach_unverified_email(existing.id).await? {
            warn!(user_id = %existing.id, "Unverified address taken over by its proven owner (Google)");
            AuditEvent::new(actions::USER_EMAIL_DETACHED)
                .target("user", existing.id, &existing.username)
                .meta(json!({ "email": mask_email(&identity.email), "reason": "proven_by_google" }))
                .ip(&ip.0)
                .record(&*state.audit_repo)
                .await;
        } else if state
            .user_repo
            .is_email_taken(&identity.email, None)
            .await?
        {
            // Verified meanwhile: same answer as a verified account.
            return Err(ApiError::account_link_required());
        }
    }

    // 3. A new account.
    if !payload.accept_terms || !payload.age_confirmed {
        return Err(signup_consent_error(&payload, &identity));
    }
    let locale = account::supported_locale(payload.locale.as_deref());
    let username =
        account::derive_username(&state, &identity.email, identity.name.as_deref()).await?;
    // The account has no usable password: the hash is of a random secret
    // nobody knows. Recovery sets a real one.
    let password_hash = hash_password(&random_token(32)).await?;
    let user = account::create_account(
        &state,
        AccountDraft {
            username,
            email: identity.email.clone(),
            password_hash,
            password_set: false,
            email_verified: true,
            first_name: identity
                .given_name
                .clone()
                .map(|n| n.chars().take(50).collect()),
            last_name: identity
                .family_name
                .clone()
                .map(|n| n.chars().take(50).collect()),
            terms_version: Some(CURRENT_TERMS_VERSION.to_string()),
            referral_code: payload.referral_code.clone(),
            marketing_opt_in: payload.marketing_opt_in,
            locale: locale.clone(),
            age_attested: true,
        },
    )
    .await?;
    state
        .security_repo
        .link_identity(user.id, "google", &identity.subject, Some(&identity.email))
        .await?;
    AuditEvent::new(actions::USER_REGISTERED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .meta(json!({
            "provider": "google",
            "referred": payload.referral_code.is_some(),
            "terms_version": CURRENT_TERMS_VERSION,
            "marketing_opt_in": payload.marketing_opt_in,
            "age_confirmed": true,
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    record_legal_acceptances(
        &*state.audit_repo,
        &account::signup_acceptances(
            user.id,
            "google_signup",
            payload.marketing_opt_in,
            &ip.0,
            account::user_agent(&headers),
        ),
    )
    .await;
    if let Err(e) = enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(user.id),
            to: identity.email.clone(),
            locale,
            template: EmailTemplate::Welcome {
                username: user.username.clone(),
            },
        },
    )
    .await
    {
        error!(user_id = %user.id, error = %e, "Could not queue the welcome e-mail");
    }
    // The e-mail is verified by Google: the referral qualifies and the
    // trial starts now.
    billing::qualify_referral(&state, user.id).await?;
    account::start_verified_trial(&state, user.id, &identity.email).await?;
    info!(user_id = %user.id, "Account created with Google");
    finish_google_sign_in(&state, user, None, true, &ip).await
}

/// `TERMS_NOT_ACCEPTED` (with `meta: { signup, email, name }`, so the
/// client can ask for consent and retry) or `AGE_CONFIRMATION_REQUIRED`.
fn signup_consent_error(payload: &GoogleSignInPayload, identity: &GoogleIdentity) -> ApiError {
    if !payload.accept_terms {
        return ApiError::rule_with_meta(
            StatusCode::BAD_REQUEST,
            codes::TERMS_NOT_ACCEPTED,
            "You must accept the Terms of Use and the Privacy Policy to create an account.",
            json!({ "signup": true, "email": identity.email, "name": identity.name }),
        );
    }
    ApiError::age_confirmation_required()
}

/// The end of a Google sign-in once the account is known. Account state
/// comes first (a suspended or deactivated account never gets a link or
/// a session), then the second factor. `link` is an identity matched by
/// e-mail: with 2FA it is only linked once the challenge succeeds,
/// otherwise right away. The password lockout doesn't apply: no password
/// is being guessed here (the 2FA challenge still counts failures).
async fn finish_google_sign_in(
    state: &AppState,
    user: User,
    link: Option<&GoogleIdentity>,
    is_new_account: bool,
    ip: &ClientIp,
) -> Result<Json<LoginOutcome>, ApiError> {
    ensure_can_sign_in(&user)?;

    if user.two_factor_enabled() {
        let challenge: TwoFactorChallengeResponse = account::create_challenge(
            state,
            &user,
            "google",
            ip.addr(),
            link.map(|identity| (identity.subject.as_str(), identity.email.as_str())),
        )
        .await?;
        return Ok(Json(LoginOutcome::TwoFactor(challenge)));
    }

    if let Some(identity) = link {
        link_google_identity(
            state,
            &user,
            &identity.subject,
            &identity.email,
            &ip.0,
            "google_sign_in",
        )
        .await?;
    }

    let response = account::issue_session(
        state,
        &user,
        user.must_change_password,
        is_new_account,
        "google",
        &ip.0,
    )
    .await?;
    info!(user_id = %user.id, "Login successful (Google)");
    Ok(Json(LoginOutcome::Session(response)))
}
