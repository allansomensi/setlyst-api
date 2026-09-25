//! Account flows shared by several endpoints: issuing sessions and
//! second-factor challenges, one-time e-mail codes, TOTP checks and
//! account creation.

use crate::{
    database::{
        AppState,
        repositories::{
            audit_repository::AuditEvent,
            security_repository::{CodeIssue, CodeLimits, SecondFactorClaim, UNKNOWN_REQUESTER},
            user_repository::NewAccount,
        },
    },
    email::{EmailTemplate, OutgoingEmail, enqueue, outbox::mask_email},
    errors::api_error::{ApiError, codes},
    models::{
        audit::actions,
        auth::{LoginResponse, TwoFactorChallengeResponse},
        communication::{Category, ChannelPrefs, CommunicationPreferences},
        notification::Notification,
        security::{CodeSentResponse, VerificationCode, VerificationPurpose},
        user::{MAX_EMAIL_LENGTH, User, canonical_email},
        user_preferences::SUPPORTED_LANGUAGES,
    },
    services::notifier::notify,
    utils::{
        codes::{lowercase_suffix, normalize_recovery_code, numeric_code, random_in},
        crypto::{decrypt, encrypt, hash_code, random_token, sha256_hex, verify_code},
        hashing::verify_password_async,
        jwt::generate_jwt,
        totp,
    },
    validations::username::{MAX_USERNAME_LENGTH, validate_username},
};
use axum::http::StatusCode;
use chrono::{Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};
use std::net::IpAddr;
use tracing::{error, warn};
use uuid::Uuid;

/// Lifetime of e-mailed codes.
pub const CODE_TTL_MINUTES: i64 = 15;
/// Lifetime of a re-authentication code (shorter: it confirms one action
/// the owner is performing right now).
pub const REAUTH_CODE_TTL_MINUTES: i64 = 10;
/// Re-authentication codes per account per hour, all networks together.
pub const REAUTH_CODES_PER_HOUR: i64 = 10;
/// Wrong guesses on the codes of one purpose, per account and 24 hours,
/// across fresh codes: past this, codes are neither checked nor issued.
pub const CODE_DAILY_FAILURE_CAP: i64 = 10;
/// Wrong passwords (or re-auth codes) confirming a sensitive action,
/// per account and window.
pub const REAUTH_MAX_FAILURES: i32 = 5;
pub const REAUTH_WINDOW_SECONDS: i64 = 15 * 60;
/// Wrong confirmations in 24 hours after which every session is signed
/// out (the session is evidently in the wrong hands).
pub const REAUTH_DAILY_FAILURES_BEFORE_SIGN_OUT: i32 = 10;
/// Wrong guesses allowed per code.
pub const CODE_MAX_ATTEMPTS: i32 = 5;
/// Minimum time between two requests of the same purpose for one account
/// (whoever makes them: at most one e-mail a minute to the owner).
pub const CODE_RESEND_SECONDS: i64 = 60;
/// Requests for codes of one purpose per account per 24 hours, all
/// networks together (the most e-mails a stranger's requests can put in
/// the owner's inbox in a day; a request while a code is live re-sends
/// the same code, see [`send_code`]).
pub const CODE_DAILY_LIMIT: i64 = 20;
/// ...and from one client network (IPv4 address or IPv6 /64): the
/// allowance of the owner is never used up by one stranger.
pub const CODE_DAILY_LIMIT_PER_NETWORK: i64 = 5;
/// Re-authentication code requests per account per hour, and per network.
pub const REAUTH_CODES_PER_HOUR_PER_NETWORK: i64 = 5;
/// A live code with less than this share of its lifetime left is replaced
/// by a fresh one instead of being re-sent (so a re-sent code is always
/// usable for a while).
const CODE_RESEND_MIN_REMAINING_FRACTION: i64 = 2;
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

/// `INVALID_CODE` without any hint of how many attempts remain (password
/// recovery: the count would tell whether the account exists).
pub fn invalid_code_plain() -> ApiError {
    ApiError::rule(
        StatusCode::BAD_REQUEST,
        codes::INVALID_CODE,
        "The code is not valid.",
    )
}

/// `EMAIL_NOT_VERIFIED` (403): the action needs a verified address.
pub fn email_not_verified() -> ApiError {
    ApiError::rule(
        StatusCode::FORBIDDEN,
        codes::EMAIL_NOT_VERIFIED,
        "Verify your e-mail address first.",
    )
}

/// Refuses accounts whose e-mail isn't verified.
pub fn ensure_email_verified(user: &User) -> Result<(), ApiError> {
    if user.email_verified() {
        Ok(())
    } else {
        Err(email_not_verified())
    }
}

/// Refuses `user_id` unless its account has a verified e-mail address
/// (`EMAIL_NOT_VERIFIED`, 403): creating bands, changing a band logo,
/// public sharing and backup import (CONTRACTS §5). Unlike
/// [`ensure_email_verified`] it needs no loaded [`User`].
pub async fn require_verified_email(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let verified: Option<bool> = sqlx::query_scalar(
        "SELECT email IS NOT NULL AND email_verified_at IS NOT NULL FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    if verified == Some(true) {
        Ok(())
    } else {
        Err(email_not_verified())
    }
}

/// The key the sign-in lockout is kept under for a client address: the
/// address itself for IPv4, its /64 for IPv6 (one subscriber usually
/// holds a whole /64, so finer keys would let them rotate freely).
pub fn ip_bucket(ip: Option<&str>) -> String {
    match ip.and_then(|ip| ip.parse::<IpAddr>().ok()) {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        None => "unknown".into(),
    }
}

/// What the audit log keeps of an identifier typed in a failed sign-in
/// for an unknown account: a masked e-mail, or a short keyed hash (it
/// may be a mistyped password or someone else's address).
pub fn mask_identifier(identifier: &str) -> String {
    let identifier = identifier.trim();
    if identifier.contains('@') {
        mask_email(identifier)
    } else {
        format!("#{}", &hash_code("audit", &identifier.to_lowercase())[..16])
    }
}

/// A staff search term as kept in the audit log: e-mail addresses
/// masked, anything else as typed (bounded).
pub fn mask_identifier_for_staff(term: &str) -> String {
    let term = term.trim();
    if term.contains('@') {
        mask_email(term)
    } else {
        term.chars().take(64).collect()
    }
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

/// Signs `user` in: resets the failure counters (of the account and of
/// the network the sign-in came from), records the login and issues the
/// session token. `method` (`password`, `google`, `two_factor`) and `ip`
/// go to the audit log.
pub async fn issue_session(
    state: &AppState,
    user: &User,
    must_change_password: bool,
    is_new_account: bool,
    method: &str,
    ip: &Option<String>,
) -> Result<LoginResponse, ApiError> {
    state
        .user_repo
        .reset_login_failures(user.id, Some(&ip_bucket(ip.as_deref())))
        .await?;
    let token = generate_jwt(user)?;
    let is_first_login = state.user_repo.mark_login(user.id).await?;
    AuditEvent::new(actions::USER_LOGIN_SUCCEEDED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .meta(json!({ "method": method }))
        .ip(ip)
        .spawn(state.audit_repo.clone());
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
///
/// `pending_link` is a Google identity (`sub`, e-mail) that is linked to
/// the account only once the second factor succeeds.
pub async fn create_challenge(
    state: &AppState,
    user: &User,
    method: &str,
    ip: Option<&str>,
    pending_link: Option<(&str, &str)>,
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
            pending_link,
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

/// Lifetime of the codes of `purpose`, in minutes.
pub fn code_ttl_minutes(purpose: VerificationPurpose) -> i64 {
    match purpose {
        VerificationPurpose::Reauth => REAUTH_CODE_TTL_MINUTES,
        _ => CODE_TTL_MINUTES,
    }
}

/// Issuance limits of the codes of `purpose`: one a minute per account,
/// [`CODE_DAILY_LIMIT_PER_NETWORK`] a day per client network and
/// [`CODE_DAILY_LIMIT`] a day per account ([`REAUTH_CODES_PER_HOUR`] an
/// hour for re-authentication codes); none at all once the day's wrong
/// guesses reach [`CODE_DAILY_FAILURE_CAP`].
pub fn code_limits(purpose: VerificationPurpose) -> CodeLimits {
    let (window_seconds, max_per_requester_in_window, max_in_window) = match purpose {
        VerificationPurpose::Reauth => (
            3600,
            REAUTH_CODES_PER_HOUR_PER_NETWORK,
            REAUTH_CODES_PER_HOUR,
        ),
        _ => (24 * 3600, CODE_DAILY_LIMIT_PER_NETWORK, CODE_DAILY_LIMIT),
    };
    CodeLimits {
        resend_seconds: CODE_RESEND_SECONDS,
        window_seconds,
        max_per_requester_in_window,
        max_in_window,
        max_failures_per_day: CODE_DAILY_FAILURE_CAP,
    }
}

/// The key code requests are counted under: the client network (IPv4
/// address, IPv6 /64), or [`UNKNOWN_REQUESTER`] when it isn't known.
pub fn code_requester(ip: Option<&str>) -> String {
    ip.and_then(|ip| ip.trim().parse::<IpAddr>().ok())
        .map(|ip| crate::middlewares::client_ip::rate_limit_key(ip).to_string())
        .unwrap_or_else(|| UNKNOWN_REQUESTER.to_string())
}

/// E-mails a code for `purpose` to `target_email`, enforcing the resend
/// interval and the per-network and per-account allowances (see
/// [`code_limits`]) and the daily failure cap.
///
/// While a code for the same purpose and address is still valid (with at
/// least half its lifetime left), the *same* code is sent again rather
/// than a new one issued: password recovery takes anyone's request for
/// any account, and a stranger's request must not invalidate the code
/// the owner is about to type. The code is kept encrypted at rest for
/// that (`verification_codes.code_enc`), and wiped once consumed.
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
    let ttl = Duration::minutes(code_ttl_minutes(purpose));
    let fresh = numeric_code();
    let (code, expires_at) = match state
        .security_repo
        .create_code(
            user.id,
            purpose,
            &hash_code(purpose.key(), &fresh),
            &encrypt(fresh.as_bytes())?,
            target_email,
            timestamp + ttl,
            ttl / CODE_RESEND_MIN_REMAINING_FRACTION as i32,
            ip,
            &code_requester(ip),
            code_limits(purpose),
        )
        .await?
    {
        CodeIssue::Issued(_) => (fresh, timestamp + ttl),
        CodeIssue::Resent {
            code_enc,
            expires_at,
            ..
        } => {
            let stored = decrypt(&code_enc)?;
            let stored = String::from_utf8(stored)
                .map_err(|_| ApiError::ServerError(axum::Error::new("stored code is not text")))?;
            (stored, expires_at)
        }
        CodeIssue::Limited {
            retry_after_seconds,
        } => return Err(too_many_attempts(retry_after_seconds)),
    };
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
    // Fresh codes don't bring fresh guesses: past the daily cap of wrong
    // guesses on this purpose, nothing is checked any more.
    let failures = state
        .security_repo
        .code_failures_since(user_id, purpose, now() - Duration::hours(24))
        .await?;
    if failures >= CODE_DAILY_FAILURE_CAP {
        warn!(%user_id, purpose = purpose.key(), "Daily cap of wrong codes reached");
        return Err(too_many_attempts(3600));
    }
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
    state.security_repo.record_code_failure(stored.id).await?;
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
    // The short window alone never escalates: 5 guesses every 15 minutes
    // is 480 a day against a 6-digit code, forever. Wrong second factors
    // count towards the same daily total as wrong passwords and re-auth
    // codes, which signs the account out everywhere when reached (the
    // session is evidently in the wrong hands).
    let day_failures = state.security_repo.record_reauth_failure(user.id).await?;
    sign_out_after_too_many_failures(state, user, day_failures, ip).await?;
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
    /// The age declaration was made.
    pub age_attested: bool,
}

/// Creates the account (with preferences and referral) and queues the
/// automatic username review. The trial only starts once the address is
/// verified (see [`start_verified_trial`]).
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
            age_attested: draft.age_attested,
        })
        .await?;
    crate::moderation::spawn_username_review(state, user.id, &user.username);
    Ok(user)
}

/// The keyed hash a trial is claimed under: the canonical form of the
/// address (see [`canonical_email`]), so `+tags`, Gmail dots and case
/// don't make it a "new" address.
pub fn trial_email_hash(email: &str) -> Vec<u8> {
    let hex = hash_code("trial", &canonical_email(email));
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Starts the sign-up trial (when plans are enforced) once the account
/// proved it owns `email`: e-mail verification, a password recovery that
/// verified the address, or a Google sign-up. One trial per canonical
/// address, ever: the claim survives the account's deletion.
pub async fn start_verified_trial(
    state: &AppState,
    user_id: Uuid,
    email: &str,
) -> Result<(), ApiError> {
    let hash = trial_email_hash(email);
    if !state.security_repo.claim_trial(&hash).await? {
        tracing::info!(%user_id, "Trial already used by this address; none started");
        return Ok(());
    }
    crate::services::billing::start_registration_trial(state, user_id).await?;
    // The claim is only kept when a trial actually started (plans may not
    // be enforced, or the account may already be on a plan).
    let started: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM subscriptions
                        WHERE user_id = $1 AND source = 'trial' AND status = 'trialing')",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
    if !started {
        state.security_repo.release_trial_claim(&hash).await?;
    }
    Ok(())
}

/// Queues a [`EmailTemplate::SecurityNotice`] of `kind` to `to` (the
/// account's address when `None` — only once it is verified: nothing but
/// codes is ever sent to an address nobody has proven, or a settings
/// change looped on a throw-away account would mail whoever it names),
/// in the account's language. Never fails the request.
pub async fn send_security_notice(
    state: &AppState,
    user: &User,
    kind: &str,
    detail: Option<String>,
    to: Option<&str>,
) {
    let Some(to) = to
        .map(str::to_string)
        .or_else(|| user.email.clone().filter(|_| user.email_verified()))
    else {
        return;
    };
    let locale = user_locale(state, user.id)
        .await
        .unwrap_or_else(|_| "en".into());
    if let Err(e) = enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(user.id),
            to,
            locale,
            template: EmailTemplate::SecurityNotice {
                username: user.username.clone(),
                kind: kind.to_string(),
                detail,
            },
        },
    )
    .await
    {
        error!(user_id = %user.id, error = %e, "Could not queue a security notice");
    }
}

/// A fresh proof of identity for a sensitive action.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReauthProof<'a> {
    /// The current password (accounts with a password).
    pub password: Option<&'a str>,
    /// A code from `POST /users/me/reauth/code` (accounts without one).
    pub reauth_code: Option<&'a str>,
}

/// Counts a wrong re-authentication proof: audited, and past
/// [`REAUTH_DAILY_FAILURES_BEFORE_SIGN_OUT`] in 24 hours every session is
/// signed out and the owner is told.
async fn record_reauth_failure(
    state: &AppState,
    user: &User,
    context: &str,
    ip: &Option<String>,
) -> Result<(), ApiError> {
    let day_failures = state.security_repo.record_reauth_failure(user.id).await?;
    AuditEvent::new(actions::USER_REAUTH_FAILED)
        .actor(user.id, &user.username)
        .target("user", user.id, &user.username)
        .meta(json!({ "context": context, "failures_24h": day_failures }))
        .ip(ip)
        .spawn(state.audit_repo.clone());
    sign_out_after_too_many_failures(state, user, day_failures, ip).await
}

/// Every wrong confirmation of a sensitive action (a password, a re-auth
/// code or a second factor) counts towards one 24-hour total; at
/// [`REAUTH_DAILY_FAILURES_BEFORE_SIGN_OUT`] every session is signed out
/// and the owner is told. `day_failures` is that total after the failure
/// just recorded.
async fn sign_out_after_too_many_failures(
    state: &AppState,
    user: &User,
    day_failures: i32,
    ip: &Option<String>,
) -> Result<(), ApiError> {
    if day_failures == REAUTH_DAILY_FAILURES_BEFORE_SIGN_OUT {
        warn!(user_id = %user.id, "Too many wrong confirmations; signing the account out everywhere");
        state.user_repo.revoke_sessions(user.id).await?;
        AuditEvent::new(actions::USER_REAUTH_SESSIONS_REVOKED)
            .target("user", user.id, &user.username)
            .meta(json!({ "failures_24h": day_failures }))
            .ip(ip)
            .record(&*state.audit_repo)
            .await;
        send_security_notice(state, user, "reauth_sessions_revoked", None, None).await;
        notify(
            state,
            Notification::security_alert(user.id, "reauth_sessions_revoked"),
        )
        .await;
    }
    Ok(())
}

/// Checks `password` as a confirmation of a sensitive action by the
/// signed-in `user`, through the per-account limiter
/// ([`REAUTH_MAX_FAILURES`] wrong passwords per 15 minutes, then
/// `TOO_MANY_ATTEMPTS`): a stolen session must not become an unthrottled
/// password oracle. `WRONG_PASSWORD` when wrong.
pub async fn check_reauth_password(
    state: &AppState,
    user: &User,
    password: &str,
    context: &str,
    ip: &Option<String>,
) -> Result<(), ApiError> {
    if let SecondFactorClaim::Limited {
        retry_after_seconds,
    } = state
        .security_repo
        .claim_reauth_check(user.id, REAUTH_MAX_FAILURES, REAUTH_WINDOW_SECONDS)
        .await?
    {
        warn!(user_id = %user.id, %context, "Re-authentication checks limited");
        return Err(too_many_attempts(retry_after_seconds));
    }
    match verify_password_async(password, &user.password_hash).await {
        Ok(()) => {
            state.security_repo.release_reauth_check(user.id).await?;
            Ok(())
        }
        Err(ApiError::WrongPassword) => {
            record_reauth_failure(state, user, context, ip).await?;
            Err(ApiError::WrongPassword)
        }
        Err(e) => {
            // Not the user's fault (e.g. `SERVICE_BUSY`): give it back.
            state.security_repo.release_reauth_check(user.id).await?;
            Err(e)
        }
    }
}

/// Requires a fresh proof of identity before a sensitive action (e-mail
/// change, 2FA setup or removal, unlinking a sign-in provider, account
/// deletion): the password for accounts that have one, else a code
/// e-mailed to the verified address by `POST /users/me/reauth/code`.
///
/// Missing proof: `REAUTH_REQUIRED` (403, `meta.method` = `password` or
/// `email_code`). Wrong password: `WRONG_PASSWORD`; wrong code:
/// `INVALID_CODE`. Both go through the per-account limiter
/// (`TOO_MANY_ATTEMPTS`).
pub async fn require_recent_auth(
    state: &AppState,
    user: &User,
    proof: ReauthProof<'_>,
    context: &str,
    ip: &Option<String>,
) -> Result<(), ApiError> {
    if user.password_set {
        return match proof.password.filter(|p| !p.is_empty()) {
            Some(password) => check_reauth_password(state, user, password, context, ip).await,
            None => Err(ApiError::reauth_required("password")),
        };
    }
    let Some(code) = proof.reauth_code.map(str::trim).filter(|c| !c.is_empty()) else {
        return Err(ApiError::reauth_required("email_code"));
    };
    if let SecondFactorClaim::Limited {
        retry_after_seconds,
    } = state
        .security_repo
        .claim_reauth_check(user.id, REAUTH_MAX_FAILURES, REAUTH_WINDOW_SECONDS)
        .await?
    {
        return Err(too_many_attempts(retry_after_seconds));
    }
    match consume_code(state, user.id, VerificationPurpose::Reauth, code).await {
        Ok(stored) => {
            state.security_repo.release_reauth_check(user.id).await?;
            // The code proves control of the address it was sent to,
            // which must still be the account's verified address.
            let current = user.email.as_deref().filter(|_| user.email_verified());
            if !current.is_some_and(|e| e.eq_ignore_ascii_case(&stored.target_email)) {
                return Err(invalid_code(0));
            }
            Ok(())
        }
        Err(e) => {
            if e.code() == codes::INVALID_CODE || e.code() == codes::CODE_EXPIRED {
                record_reauth_failure(state, user, context, ip).await?;
            } else {
                state.security_repo.release_reauth_check(user.id).await?;
            }
            Err(e)
        }
    }
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

/// The `User-Agent` of a request (bounded), kept as consent evidence.
pub fn user_agent(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(255).collect())
}

/// Ledger rows for the consents given at sign-up: the Terms of Use and
/// the Privacy Policy in force, the age declaration, and the marketing
/// choice (recorded either way).
pub fn signup_acceptances(
    user_id: Uuid,
    source: &'static str,
    marketing_opt_in: bool,
    ip: &Option<String>,
    user_agent: Option<String>,
) -> Vec<crate::models::audit::LegalAcceptance> {
    use crate::models::{audit::legal_documents as docs, user::CURRENT_TERMS_VERSION};
    [
        (docs::TERMS_OF_USE, true),
        (docs::PRIVACY_POLICY, true),
        (docs::AGE_DECLARATION, true),
        (docs::MARKETING_EMAIL, marketing_opt_in),
    ]
    .into_iter()
    .map(
        |(document, accepted)| crate::models::audit::LegalAcceptance {
            user_id,
            document,
            version: CURRENT_TERMS_VERSION.to_string(),
            accepted,
            source,
            ip_address: ip.clone(),
            user_agent: user_agent.clone(),
        },
    )
    .collect()
}

/// Sends the "sign-in attempts blocked" notice, at most once an hour per
/// account (an attacker rotating networks must not turn it into spam).
pub async fn notify_login_locked(state: &AppState, user: &User) {
    let recent: Result<bool, sqlx::Error> = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM email_outbox
                        WHERE user_id = $1 AND template = 'security_notice'
                          AND payload->>'kind' = 'login_locked'
                          AND created_at > (NOW() AT TIME ZONE 'utc') - INTERVAL '1 hour')",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await;
    if matches!(recent, Ok(false)) {
        send_security_notice(state, user, "login_locked", None, None).await;
        notify(state, Notification::security_alert(user.id, "login_locked")).await;
    }
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
    fn lockout_buckets_group_ipv6_by_64() {
        assert_eq!(ip_bucket(Some("203.0.113.7")), "203.0.113.7");
        assert_eq!(
            ip_bucket(Some("2001:db8:1:2:aaaa::1")),
            ip_bucket(Some("2001:db8:1:2:bbbb::9"))
        );
        assert_ne!(
            ip_bucket(Some("2001:db8:1:2::1")),
            ip_bucket(Some("2001:db8:1:3::1"))
        );
        assert_eq!(ip_bucket(Some("::ffff:203.0.113.7")), "203.0.113.7");
        assert_eq!(ip_bucket(None), "unknown");
    }

    #[test]
    fn failed_sign_in_identifiers_are_masked() {
        assert_eq!(
            mask_identifier("ana@example.com"),
            mask_email("ana@example.com")
        );
        let hashed = mask_identifier("Hunter2!");
        assert!(hashed.starts_with('#') && hashed.len() == 17);
        assert!(!hashed.contains("Hunter2"));
        assert_eq!(mask_identifier("Ghost"), mask_identifier("ghost"));
    }

    #[test]
    fn trial_hashes_follow_the_canonical_address() {
        assert_eq!(
            trial_email_hash("A.na+1@gmail.com"),
            trial_email_hash("ana@googlemail.com")
        );
        assert_ne!(
            trial_email_hash("ana@example.com"),
            trial_email_hash("bia@example.com")
        );
        assert_eq!(trial_email_hash("ana@example.com").len(), 32);
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
