use crate::{
    errors::{api_error::ApiError, auth_error::AuthError},
    models::user::Status,
    utils::jwt::decode_jwt,
};
use axum::{
    body::Body,
    extract::Request,
    http::{self, Response},
    middleware::Next,
};

pub async fn authenticate(mut req: Request, next: Next) -> Result<Response<Body>, ApiError> {
    let token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .filter(|header_str| header_str.starts_with("Bearer "))
        .map(|header_str| header_str.trim_start_matches("Bearer "))
        .ok_or(ApiError::from(AuthError::MissingToken))?;

    let token_data =
        decode_jwt(token.to_string()).map_err(|_| ApiError::from(AuthError::InvalidToken))?;

    if token_data.claims.status != Status::Active {
        return Err(ApiError::Unauthorized);
    }

    req.extensions_mut().insert(token_data.claims);

    Ok(next.run(req).await)
}
