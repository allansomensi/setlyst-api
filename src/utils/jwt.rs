use crate::{
    config::Config,
    errors::api_error::ApiError,
    models::{auth::token::Claims, user::User},
};
use chrono::{Duration, TimeDelta, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode};

pub fn generate_jwt(user: &User) -> Result<String, ApiError> {
    let config = Config::get();
    let now = Utc::now();
    let expire: TimeDelta = Duration::seconds(config.jwt_expiration_time);
    let exp: usize = (now + expire).timestamp() as usize;
    let iat: usize = now.timestamp() as usize;

    let claims = Claims {
        sub: user.id,
        username: user.username.clone(),
        role: user.role.clone(),
        status: user.status.clone(),
        exp,
        iat,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )?;

    Ok(token)
}

pub fn validate_jwt(token: &str) -> Result<(), ApiError> {
    let config = Config::get();
    let validation = Validation::default();
    let _: TokenData<Claims> = decode(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &validation,
    )?;
    Ok(())
}

pub fn decode_jwt(token: String) -> Result<TokenData<Claims>, ApiError> {
    let config = Config::get();
    Ok(decode::<Claims>(
        &token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )?)
}
