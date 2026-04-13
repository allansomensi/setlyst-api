use crate::{
    database::AppState,
    errors::{api_error::ApiError, auth_error::AuthError},
    models::user::User,
    utils::jwt::decode_jwt,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{self, Response},
    middleware::Next,
};
use std::sync::Arc;

pub async fn authenticate(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response<Body>, ApiError> {
    let auth_header = req.headers_mut().get(http::header::AUTHORIZATION);

    let auth_header = match auth_header {
        Some(header) => header
            .to_str()
            .map_err(|_| ApiError::from(AuthError::EmptyHeader)),
        None => return Err(ApiError::from(AuthError::MissingToken)),
    };

    let mut header = auth_header?.split_whitespace();

    let _bearer = header.next();

    let token = header
        .next()
        .ok_or(ApiError::from(AuthError::MissingToken))?;

    let token_data = match decode_jwt(token.to_string()) {
        Ok(data) => data,
        Err(_) => return Err(ApiError::from(AuthError::InvalidToken)),
    };

    let current_user: User = sqlx::query_as(r#"SELECT * FROM users WHERE username = $1;"#)
        .bind(&token_data.claims.sub)
        .fetch_one(&state.db)
        .await?;

    req.extensions_mut().insert(current_user);
    Ok(next.run(req).await)
}
