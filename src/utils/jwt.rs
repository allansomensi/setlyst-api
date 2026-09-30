use crate::{
    config::Config,
    database::repositories::user_repository::AuthState,
    errors::{api_error::ApiError, auth_error::AuthError},
    models::{auth::token::Claims, user::User},
};
use chrono::{Duration, NaiveDateTime, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode,
};
use std::sync::OnceLock;
use uuid::Uuid;

/// Clock skew tolerated on `exp` between this server and whoever checks
/// the token (the web server's `/auth/verify` calls).
const LEEWAY_SECONDS: u64 = 30;
/// A token claiming to be issued further in the future than this is
/// rejected: it can only come from a forged or badly clocked issuer.
const MAX_FUTURE_IAT_SECONDS: i64 = 60;

/// The signing and verification material, derived from the secret once:
/// every authenticated request verifies a token, and rebuilding the key
/// and the validation rules (two sets and a copy of the secret) each time
/// is pure waste on that path.
struct Keys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let secret = Config::get().jwt_secret.as_bytes();
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = LEEWAY_SECONDS;
        Keys {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
        }
    })
}

fn sign(claims: &Claims) -> Result<String, ApiError> {
    Ok(encode(
        &Header::new(Algorithm::HS256),
        claims,
        &keys().encoding,
    )?)
}

fn build_claims(user: &User, ttl_seconds: i64, impersonator: Option<(Uuid, i32)>) -> Claims {
    let now = Utc::now();
    Claims {
        sub: user.id,
        username: user.username.clone(),
        role: user.role.clone(),
        status: user.status.clone(),
        exp: (now + Duration::seconds(ttl_seconds)).timestamp() as usize,
        iat: now.timestamp() as usize,
        ver: user.token_version,
        imp: impersonator.map(|(id, _)| id),
        iver: impersonator.map(|(_, version)| version),
    }
}

/// A regular session token for `user`.
pub fn generate_jwt(user: &User) -> Result<String, ApiError> {
    let config = Config::get();
    sign(&build_claims(user, config.jwt_expiration_time, None))
}

/// A fresh session token for the account a still-valid token belongs to,
/// with the account's *current* username, role, status and
/// `token_version` (the caller has already checked it isn't revoked).
/// Returns the token and its expiry.
pub fn renew_jwt(user_id: Uuid, account: &AuthState) -> Result<(String, NaiveDateTime), ApiError> {
    let config = Config::get();
    let now = Utc::now();
    let claims = Claims {
        sub: user_id,
        username: account.username.clone(),
        role: account.role.clone(),
        status: account.status.clone(),
        exp: (now + Duration::seconds(config.jwt_expiration_time)).timestamp() as usize,
        iat: now.timestamp() as usize,
        ver: account.token_version,
        imp: None,
        iver: None,
    };
    let expires_at = chrono::DateTime::from_timestamp(claims.exp as i64, 0)
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| now.naive_utc());
    Ok((sign(&claims)?, expires_at))
}

/// A short-lived, read-only token that lets `impersonator_id` see the
/// platform as `target`. `impersonator_token_version` binds the token to
/// the staff member's current sessions. Returns the token and its expiry.
pub fn generate_impersonation_jwt(
    target: &User,
    impersonator_id: Uuid,
    impersonator_token_version: i32,
) -> Result<(String, NaiveDateTime), ApiError> {
    let config = Config::get();
    let claims = build_claims(
        target,
        config.impersonation_expiration_time,
        Some((impersonator_id, impersonator_token_version)),
    );
    let expires_at = chrono::DateTime::from_timestamp(claims.exp as i64, 0)
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| Utc::now().naive_utc());
    Ok((sign(&claims)?, expires_at))
}

pub fn validate_jwt(token: &str) -> Result<(), ApiError> {
    decode_jwt(token).map(|_| ())
}

/// Decodes and validates a session token: HS256 only (the algorithm is
/// pinned, never taken from the token header), `exp` with a small
/// leeway, and an `iat` that is not in the future.
pub fn decode_jwt(token: &str) -> Result<TokenData<Claims>, ApiError> {
    let keys = keys();
    let data = decode::<Claims>(token, &keys.decoding, &keys.validation)?;

    if data.claims.iat as i64 > Utc::now().timestamp() + MAX_FUTURE_IAT_SECONDS {
        return Err(ApiError::from(AuthError::InvalidToken));
    }
    Ok(data)
}
