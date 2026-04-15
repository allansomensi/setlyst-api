use crate::database::AppState;
use crate::errors::api_error::ApiError;
use tracing::error;
use uuid::Uuid;

/// Check if there is already another artist with the same name.
pub async fn is_artist_unique(state: &AppState, name: &str, user_id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM artists WHERE name = $1 AND user_id = $2;")
        .bind(name)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if exists {
        error!("Artist '{name}' already exists for this user.");
        Err(ApiError::AlreadyExists)
    } else {
        Ok(())
    }
}
