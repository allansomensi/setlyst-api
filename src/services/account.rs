//! Account flows shared by several endpoints: issuing sessions and
//! second-factor challenges, one-time e-mail codes, TOTP checks and
//! account creation.

use crate::{
    database::{
        AppState,
        repositories::{
            audit_repository::AuditEvent, security_repository::SecondFactorClaim,
            user_repository::NewAccount,
        },
    },
    email::{EmailTemplate, OutgoingEmail, enqueue},
    errors::api_error::{ApiError, codes},
    models::{
        audit::actions,
        auth::{LoginResponse, TwoFactorChallengeResponse},
        communication::{Category, ChannelPrefs, CommunicationPreferences},
        security::{CodeSentResponse, VerificationCode, VerificationPurpose},
        user::{MAX_EMAIL_LENGTH, User},
        user_preferences::SUPPORTED_LANGUAGES,
    },
    utils::{
        codes::{lowercase_suffix, normalize_recovery_code, numeric_code, random_in},
        crypto::{decrypt, hash_code, random_token, sha256_hex, verify_code},
        jwt::generate_jwt,
        totp,
    },
    validations::username::{MAX_USERNAME_LENGTH, validate_username},
};
use axum::http::StatusCode;
use chrono::{Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};
use tracing::{error, warn};
use uuid::Uuid;

/// Lifetime of e-mailed codes.
pub const CODE_TTL_MINUTES: i64 = 15;
/// Wrong guesses allowed per code.
pub const CODE_MAX_ATTEMPTS: i32 = 5;
/// Minimum time between two codes of the same purpose.
pub const CODE_RESEND_SECONDS: i64 = 60;
/// Codes of one purpose per account per 24 hours.
pub const CODE_DAILY_LIMIT: i64 = 10;
/// Lifetime of a second-factor sign-in challenge.
pub const CHALLENGE_TTL_MINUTES: i64 = 5;
pub const CHALLENGE_MAX_ATTEMPTS: i32 = 5;
/// Consecutive failed sign-ins before the account is locked.
pub const LOCKOUT_THRESHOLD: i32 = 5;
/// A pending 2FA secret must be confirmed within this time.
pub const TOTP_SETUP_TTL_MINUTES: i64 = 15;
/// Failed second-factor checks outside sign-in (disabling 2FA,
/// regenerating recovery codes) allowed per account and window.
pub const SECOND_FACTOR_MAX_FAILURES: i32 = 5;
pub const SECOND_FACTOR_WINDOW_SECONDS: i64 = 15 * 60;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// `ACCOUNT_LOCKED` (429) with the end of the lock.
pub fn account_locked(until: NaiveDateTime) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::TOO_MANY_REQUESTS,
        codes::ACCOUNT_LOCKED,
        "Too many failed attempts. This account is temporarily locked.",
        json!({ "until": until }),
    )
}

pub fn too_many_attempts(retry_after_seconds: i64) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::TOO_MANY_REQUESTS,
        codes::TOO_MANY_ATTEMPTS,
        "Too many attempts. Please wait before trying again.",
        json!({ "retry_after_seconds": retry_after_seconds.max(1) }),
    )
}

pub fn invalid_code(attempts_left: i32) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::BAD_REQUEST,
        codes::INVALID_CODE,
        "The code is not valid.",
        json!({ "attempts_left": attempts_left.max(0) }),
    )
}

pub fn code_expired() -> ApiError {
    ApiError::rule(
        StatusCode::BAD_REQUEST,
        codes::CODE_EXPIRED,
        "The code has expired. Request a new one.",
    )
}

pub fn invalid_two_factor_code(attempts_left: Option<i32>) -> ApiError {
    match attempts_left {
        Some(left) => ApiError::rule_with_meta(
            StatusCode::UNAUTHORIZED,
            codes::INVALID_TWO_FACTOR_CODE,
            "The verification code is not valid.",
            json!({ "attempts_left": left.max(0) }),
        ),
        None => ApiError::rule(
            StatusCode::UNAUTHORIZED,
            codes::INVALID_TWO_FACTOR_CODE,
            "The verification code is not valid.",
        ),
    }
}

/// `TWO_FACTOR_UNAVAILABLE` (500): a stored 2FA secret can't be read.
pub fn two_factor_unavailable() -> ApiError {
    ApiError::rule(
        StatusCode::INTERNAL_SERVER_ERROR,
        codes::TWO_FACTOR_UNAVAILABLE,
        "Two-factor authentication is temporarily unavailable. Please contact support.",
    )
}

/// Decrypts a stored TOTP secret of `user_id`. A failure means the data
/// encryption key changed since the secret was stored (or, with a derived
/// key, `JWT_SECRET` was rotated): nothing the client can fix, so it gets
/// a stable code while the operator gets the cause in the log.
pub fn decrypt_totp_secret(user_id: Uuid, stored: &str) -> Result<Vec<u8>, ApiError> {
    decrypt(stored).map_err(|_| {
        error!(
            %user_id,
            "Stored two-factor secret can't be decrypted: DATA_ENCRYPTION_KEY (or JWT_SECRET, when the key is derived) changed since it was stored"
        );
        two_factor_unavailable()
    })
}

/// Signs `user` in: resets the failure counter, records the login and
/// issues the session token.
pub async fn issue_session(
    state: &AppState,
    user: &User,
    must_change_password: bool,
    is_new_account: bool,
) -> Result<LoginResponse, ApiError> {
    state.user_repo.reset_login_failures(user.id).await?;
    let token = generate_jwt(user)?;
    let is_first_login = state.user_repo.mark_login(user.id).await?;
    Ok(LoginResponse {
        token,
        is_first_login,
        must_change_password,
        two_factor_required: false,
        terms_accepted: user.terms_accepted(),
        email_verified: user.email_verified(),
        is_new_account,
    })
}

/// Creates the second step of a sign-in for an account with 2FA.
pub async fn create_challenge(
    state: &AppState,
    user: &User,
    method: &str,
    ip: Option<&str>,
) -> Result<TwoFactorChallengeResponse, ApiError> {
    let token = random_token(32);
    let expires_at = now() + Duration::minutes(CHALLENGE_TTL_MINUTES);
    state
        .security_repo
        .create_challenge(
            user.id,
            &sha256_hex(token.as_bytes()),
            method,
            expires_at,
            ip,
        )
        .await?;
    Ok(TwoFactorChallengeResponse {
        two_factor_required: true,
        challenge_token: token,
        challenge_expires_at: expires_at,
    })
}

/// Checks a TOTP `code` for `user` and claims its time step (a code works
/// once). `false` when wrong or replayed.
pub async fn check_totp(state: &AppState, user: &User, code: &str) -> Result<bool, ApiError> {
    let Some(secret_enc) = &user.totp_secret_enc else {
        return Ok(false);
    };
    let secret = decrypt_totp_secret(user.id, secret_enc)?;
    let unix = Utc::now().timestamp().max(0) as u64;
    let Some(step) = totp::verify(&secret, code, unix, user.totp_last_step) else {
        return Ok(false);
    };
    state.user_repo.claim_totp_step(user.id, step).await
}

/// Uses one of `user`'s recovery codes. `false` when none matches.
pub async fn use_recovery_code(
    state: &AppState,
    user: &User,
    code: &str,
) -> Result<bool, ApiError> {
    let Some(normalized) = normalize_recovery_code(code) else {
        return Ok(false);
    };
    state
        .security_repo
        .use_recovery_code(user.id, &hash_code("recovery", &normalized))
        .await
}

/// A second factor: a 6-digit app code, or (when it doesn't look like
/// one) a recovery code.
pub async fn check_second_factor(
    state: &AppState,
    user: &User,
    code: &str,
) -> Result<bool, ApiError> {
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() == 6 && compact.chars().all(|c| c.is_ascii_digit()) {
        check_totp(state, user, &compact).await
    } else {
        use_recovery_code(state, user, &compact).await
    }
}

/// Issues a new e-mailed code, enforcing the resend interval and the
/// daily limit, and enqueues the e-mail built by `template`.
pub async fn send_code(
    state: &AppState,
    user: &User,
    purpose: VerificationPurpose,
    target_email: &str,
    locale: &str,
    ip: Option<&str>,
    template: impl FnOnce(String) -> EmailTemplate,
) -> Result<CodeSentResponse, ApiError> {
    let timestamp = now();
    let (last, today) = state
        .security_repo
        .code_issuance(user.id, purpose, timestamp - Duration::hours(24))
        .await?;
    if let Some(last) = last {
        let wait = CODE_RESEND_SECONDS - (timestamp - last).num_seconds();
        if wait > 0 {
            return Err(too_many_attempts(wait));
        }
    }
    if today >= CODE_DAILY_LIMIT {
        return Err(too_many_attempts(3600));
    }

    let code = numeric_code();
    let expires_at = timestamp + Duration::minutes(CODE_TTL_MINUTES);
    state
        .security_repo
        .create_code(
            user.id,
            purpose,
            &hash_code(purpose.key(), &code),
            target_email,
            expires_at,
            ip,
        )
        .await?;
    enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(user.id),
            to: target_email.to_string(),
            locale: locale.to_string(),
            template: template(code),
        },
    )
    .await?;
    Ok(CodeSentResponse {
        expires_at,
        resend_after_seconds: CODE_RESEND_SECONDS,
    })
}

/// Checks `code` against the user's current code for `purpose` and
/// consumes it. Returns the consumed code on success.
pub async fn consume_code(
    state: &AppState,
    user_id: Uuid,
    purpose: VerificationPurpose,
    code: &str,
) -> Result<VerificationCode, ApiError> {
    let stored = check_code(state, user_id, purpose, code).await?;
    // Only one of several concurrent correct attempts gets to use it.
    if !state.security_repo.consume_code(stored.id).await? {
        return Err(invalid_code(0));
    }
    Ok(stored)
}

/// Checks `code` against the user's current code for `purpose` without
/// consuming it (the caller consumes it with
/// [`crate::database::repositories::security_repository::SecurityRepository::consume_code`]
/// once the rest of the request is valid, or gives the attempt back with
/// `release_code_attempt`).
///
/// The attempt is claimed atomically *before* the comparison, so
/// concurrent guesses can never exceed [`CODE_MAX_ATTEMPTS`]: `INVALID_CODE`
/// (with `attempts_left`) or `CODE_EXPIRED` otherwise.
pub async fn check_code(
    state: &AppState,
    user_id: Uuid,
    purpose: VerificationPurpose,
    code: &str,
) -> Result<VerificationCode, ApiError> {
    let Some(stored) = state.security_repo.latest_code(user_id, purpose).await? else {
        return Err(invalid_code(0));
    };
    let Some(attempts) = state
        .security_repo
        .claim_code_attempt(stored.id, CODE_MAX_ATTEMPTS)
        .await?
    else {
        // Nothing left to claim: out of attempts (possibly just now, by a
        // concurrent request), consumed meanwhile, or expired.
        return Err(
            if stored.attempts < CODE_MAX_ATTEMPTS && stored.expires_at <= now() {
                code_expired()
            } else {
                invalid_code(0)
            },
        );
    };
    if verify_code(purpose.key(), code.trim(), &stored.code_hash) {
        return Ok(stored);
    }
    if attempts >= CODE_MAX_ATTEMPTS {
        state.security_repo.consume_code(stored.id).await?;
    }
    Err(invalid_code(CODE_MAX_ATTEMPTS - attempts))
}

/// A second factor outside sign-in (disabling 2FA, regenerating the
/// recovery codes), limited to [`SECOND_FACTOR_MAX_FAILURES`] failures
/// per [`SECOND_FACTOR_WINDOW_SECONDS`] per account: a stolen session
/// must not become a way to brute-force the 6-digit code. The check is
/// claimed before verifying (so concurrent guesses count) and given back
/// when the code is right. `TOO_MANY_ATTEMPTS` (with
/// `retry_after_seconds`) when out of attempts, `INVALID_TWO_FACTOR_CODE`
/// (with `attempts_left`) when wrong.
pub async fn require_second_factor(
    state: &AppState,
    user: &User,
    code: &str,
    context: &str,
    ip: &Option<String>,
) -> Result<(), ApiError> {
    let failures = match state
        .security_repo
        .claim_second_factor_check(
            user.id,
            SECOND_FACTOR_MAX_FAILURES,
            SECOND_FACTOR_WINDOW_SECONDS,
        )
        .await?
    {
        SecondFactorClaim::Allowed { failures } => failures,
        SecondFactorClaim::Limited {
            retry_after_seconds,
        } => {
            warn!(user_id = %user.id, %context, "Second-factor checks limited");
            return Err(too_many_attempts(retry_after_seconds));
        }
    };
    if check_second_factor(state, user, code).await? {
        state
            .security_repo
            .release_second_factor_check(user.id)
            .await?;
        return Ok(());
    }
    AuditEvent::new(actions::USER_SECOND_FACTOR_FAILED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .meta(json!({ "context": context, "failures": failures }))
        .ip(ip)
        .spawn(state.audit_repo.clone());
    Err(invalid_two_factor_code(Some(
        SECOND_FACTOR_MAX_FAILURES - failures,
    )))
}

/// The user's UI language, used for e-mails (`en` when unknown).
pub async fn user_locale(state: &AppState, user_id: Uuid) -> Result<String, ApiError> {
    let (_, language) = state.user_prefs_repo.get_communication(user_id).await?;
    Ok(language
        .filter(|l| SUPPORTED_LANGUAGES.contains(&l.as_str()))
        .unwrap_or_else(|| "en".to_string()))
}

/// A supported locale, or `en`.
pub fn supported_locale(locale: Option<&str>) -> String {
    locale
        .filter(|l| SUPPORTED_LANGUAGES.contains(l))
        .unwrap_or("en")
        .to_string()
}

/// Initial communication preferences of a new account.
pub fn initial_communication(marketing_opt_in: bool) -> Value {
    let mut prefs = CommunicationPreferences::default();
    prefs.set(
        Category::Marketing,
        ChannelPrefs {
            email: marketing_opt_in,
            in_app: marketing_opt_in,
        },
    );
    prefs.to_stored()
}

/// Turns an e-mail local part or a display name into a username that
/// satisfies the username policy (not checked for uniqueness).
pub fn username_base(email: &str, name: Option<&str>) -> String {
    let source = email
        .split('@')
        .next()
        .filter(|s| !s.is_empty())
        .or(name)
        .unwrap_or("user");
    let mut out = String::new();
    let mut last_sep = true;
    for c in source.chars() {
        let folded = match c.to_lowercase().next().unwrap_or(c) {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        };
        if folded.is_ascii_alphanumeric() {
            out.push(folded);
            last_sep = false;
        } else if matches!(folded, '.' | '_' | '-' | ' ' | '+') && !last_sep {
            out.push(if folded == ' ' || folded == '+' {
                '.'
            } else {
                folded
            });
            last_sep = true;
        }
    }
    let mut out: String = out
        .trim_start_matches(|c: char| !c.is_ascii_alphabetic())
        .to_string();
    // Leave room for a numeric suffix.
    out.truncate(MAX_USERNAME_LENGTH - 5);
    while out.ends_with(['.', '_', '-']) {
        out.pop();
    }
    if out.len() < 3 {
        out = format!("user{out}");
    }
    if validate_username(&out).is_err() {
        out = "user".to_string();
    }
    out
}

/// A free username derived from `email`/`name` (with a numeric suffix
/// when taken).
pub async fn derive_username(
    state: &AppState,
    email: &str,
    name: Option<&str>,
) -> Result<String, ApiError> {
    let base = username_base(email, name);
    if validate_username(&base).is_ok()
        && state.user_repo.is_username_available(&base, None).await?
    {
        return Ok(base);
    }
    for _ in 0..20 {
        let candidate = format!("{base}{}", random_in(10, 10_000));
        if validate_username(&candidate).is_ok()
            && state
                .user_repo
                .is_username_available(&candidate, None)
                .await?
        {
            return Ok(candidate);
        }
    }
    Ok(format!("user-{}", lowercase_suffix(8)))
}

/// Everything needed to create an account (password or provider).
pub struct AccountDraft {
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub password_set: bool,
    pub email_verified: bool,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub terms_version: Option<String>,
    pub referral_code: Option<String>,
    pub marketing_opt_in: bool,
    pub locale: String,
}

/// Creates the account (with preferences and referral), starts the trial
/// when plans are enforced and queues the automatic username review.
pub async fn create_account(state: &AppState, draft: AccountDraft) -> Result<User, ApiError> {
    let referred_by = match draft
        .referral_code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(code) => state.user_repo.find_by_referral_code(code).await?,
        None => None,
    };
    let email: String = draft.email.trim().chars().take(MAX_EMAIL_LENGTH).collect();
    let user = state
        .user_repo
        .register(&NewAccount {
            username: draft.username,
            email,
            password_hash: draft.password_hash,
            password_set: draft.password_set,
            email_verified: draft.email_verified,
            first_name: draft.first_name,
            last_name: draft.last_name,
            terms_version: draft.terms_version,
            referred_by,
            language: draft.locale,
            communication: initial_communication(draft.marketing_opt_in),
        })
        .await?;
    crate::services::billing::start_registration_trial(state, user.id).await?;
    crate::moderation::spawn_username_review(state, user.id, &user.username);
    Ok(user)
}

/// A new username for a moderation reset: `user-xxxxxx`, unused.
pub async fn random_free_username(state: &AppState) -> Result<String, ApiError> {
    for _ in 0..20 {
        let candidate = format!("user-{}", lowercase_suffix(6));
        if state
            .user_repo
            .is_username_available(&candidate, None)
            .await?
        {
            return Ok(candidate);
        }
    }
    Ok(format!("user-{}", lowercase_suffix(10)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_derived_from_emails() {
        assert_eq!(username_base("ana.maria@example.com", None), "ana.maria");
        assert_eq!(username_base("João+band@example.com", None), "joao.band");
        assert_eq!(username_base("42@example.com", Some("Ana")), "user");
        assert_eq!(username_base("x@example.com", None), "userx");
        let long = username_base("averyveryverylongemailaddress@example.com", None);
        assert!(long.len() <= MAX_USERNAME_LENGTH - 5);
        assert!(validate_username(&long).is_ok());
        assert!(validate_username(&username_base("..__--@x.com", None)).is_ok());
    }

    #[test]
    fn new_accounts_start_with_marketing_off_unless_chosen() {
        let off = CommunicationPreferences::from_stored(&initial_communication(false));
        assert!(!off.get(Category::Marketing).email);
        let on = CommunicationPreferences::from_stored(&initial_communication(true));
        assert!(on.get(Category::Marketing).email);
        assert!(on.get(Category::Bands).in_app);
    }
}
