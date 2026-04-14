use crate::database::AppState;
use crate::errors::api_error::ApiError;
use tracing::error;

/// Check if there is already another user with the same username.
pub async fn is_user_unique(state: &AppState, username: &str) -> Result<(), ApiError> {
    let exists = sqlx::query(r#"SELECT id FROM users WHERE username = $1;"#)
        .bind(username)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if exists {
        error!("Username '{username}' already exists.");
        Err(ApiError::AlreadyExists)
    } else {
        Ok(())
    }
}

/// Check if there is already another artist with the same name.
pub async fn is_artist_unique(state: &AppState, name: &str) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM artists WHERE name = $1;")
        .bind(name)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if exists {
        error!("Artist '{name}' already exists.");
        Err(ApiError::AlreadyExists)
    } else {
        Ok(())
    }
}
