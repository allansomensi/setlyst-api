use crate::{
    config::Config,
    errors::{api_error::ApiError, auth_error::AuthError},
    models::{auth::token::Claims, user::User},
};
use chrono::{Duration, NaiveDateTime, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode,
};
use uuid::Uuid;

/// Clock skew tolerated on `exp` between this server and whoever checks
/// the token (the web server's `/auth/verify` calls).
const LEEWAY_SECONDS: u64 = 30;
/// A token claiming to be issued further in the future than this is
/// rejected: it can only come from a forged or badly clocked issuer.
const MAX_FUTURE_IAT_SECONDS: i64 = 60;

fn sign(claims: &Claims) -> Result<String, ApiError> {
    let config = Config::get();
    Ok(encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
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
    decode_jwt(token.to_string()).map(|_| ())
}

/// Decodes and validates a session token: HS256 only (the algorithm is
/// pinned, never taken from the token header), `exp` with a small
/// leeway, and an `iat` that is not in the future.
pub fn decode_jwt(token: String) -> Result<TokenData<Claims>, ApiError> {
    let config = Config::get();
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECONDS;
    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &validation,
    )?;

    if data.claims.iat as i64 > Utc::now().timestamp() + MAX_FUTURE_IAT_SECONDS {
        return Err(ApiError::from(AuthError::InvalidToken));
    }
    Ok(data)
}
