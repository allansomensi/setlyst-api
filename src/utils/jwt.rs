use crate::{errors::api_error::ApiError, models::auth::token::Claims};
use chrono::{Duration, TimeDelta, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use std::env;

pub fn generate_jwt(username: &str, role: &str) -> Result<String, ApiError> {
    let now = Utc::now();
    let expire: TimeDelta = Duration::seconds(
        env::var("JWT_EXPIRATION_TIME")?
            .parse()
            .expect("Invalid JWT_EXPIRATION_TIME value"),
    );
    let exp: usize = (now + expire).timestamp() as usize;
    let iat: usize = now.timestamp() as usize;
    let role: String = role.to_string();

    let claims = Claims {
        iat,
        sub: username.to_string(),
        exp,
        role,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(env::var("JWT_SECRET")?.as_bytes()),
    )?;

    Ok(token)
}

pub fn validate_jwt(token: &str) -> Result<(), ApiError> {
    let validation = Validation::default();
    let _: TokenData<Claims> = decode(
        token,
        &DecodingKey::from_secret(env::var("JWT_SECRET")?.as_bytes()),
        &validation,
    )?;
    Ok(())
}

pub fn decode_jwt(token: String) -> Result<TokenData<Claims>, ApiError> {
    let secret = env::var("JWT_SECRET")?;

    Ok(decode::<Claims>(
        &token,
        &DecodingKey::from_secret(secret.as_ref()),
        &Validation::default(),
    )?)
}
