use crate::{
    config::Config,
    errors::api_error::ApiError,
    models::{auth::token::Claims, user::User},
};
use chrono::{Duration, NaiveDateTime, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode};
use uuid::Uuid;

fn sign(claims: &Claims) -> Result<String, ApiError> {
    let config = Config::get();
    Ok(encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )?)
}

fn build_claims(user: &User, ttl_seconds: i64, impersonator: Option<Uuid>) -> Claims {
    let now = Utc::now();
    Claims {
        sub: user.id,
        username: user.username.clone(),
        role: user.role.clone(),
        status: user.status.clone(),
        exp: (now + Duration::seconds(ttl_seconds)).timestamp() as usize,
        iat: now.timestamp() as usize,
        ver: user.token_version,
        imp: impersonator,
    }
}

/// A regular session token for `user`.
pub fn generate_jwt(user: &User) -> Result<String, ApiError> {
    let config = Config::get();
    sign(&build_claims(user, config.jwt_expiration_time, None))
}

/// A short-lived, read-only token that lets `impersonator_id` see the
/// platform as `target`. Returns the token and its expiry.
pub fn generate_impersonation_jwt(
    target: &User,
    impersonator_id: Uuid,
) -> Result<(String, NaiveDateTime), ApiError> {
    let config = Config::get();
    let claims = build_claims(
        target,
        config.impersonation_expiration_time,
        Some(impersonator_id),
    );
    let expires_at = chrono::DateTime::from_timestamp(claims.exp as i64, 0)
        .map(|dt| dt.naive_utc())
        .unwrap_or_else(|| Utc::now().naive_utc());
    Ok((sign(&claims)?, expires_at))
}

pub fn validate_jwt(token: &str) -> Result<(), ApiError> {
    decode_jwt(token.to_string()).map(|_| ())
}

pub fn decode_jwt(token: String) -> Result<TokenData<Claims>, ApiError> {
    let config = Config::get();
    Ok(decode::<Claims>(
        &token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )?)
}
