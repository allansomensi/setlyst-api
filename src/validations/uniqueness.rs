use crate::database::AppState;
use crate::errors::api_error::ApiError;
use tracing::error;

/// Check if there is already another user with the same username.
pub async fn is_user_unique(state: &AppState, username: &str) -> Result<(), ApiError> {
    let exists = sqlx::query(r#"SELECT id FROM users WHERE username = $1;"#)
        .bind(&username)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if exists {
        error!("Username '{}' already exists.", &username);
        Err(ApiError::AlreadyExists)
    } else {
        Ok(())
    }
}