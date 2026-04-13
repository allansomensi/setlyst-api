use crate::{database::AppState, errors::api_error::ApiError};
use tracing::error;
use uuid::Uuid;

/// Checks if the user is already registered according to his ID.
pub async fn user_exists(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query(r#"SELECT id FROM users WHERE id = $1;"#)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        error!("User ID not found.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}