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

/// Checks if the artist is already registered according to his ID.
pub async fn artist_exists(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM artists WHERE id = $1;")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        error!("Artist ID not found.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}

/// Checks if the song is already registered according to his ID.
pub async fn song_exists(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM songs WHERE id = $1;")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        error!("Song ID not found.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}

/// Checks if the setlist is already registered according to his ID.
pub async fn setlist_exists(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query(r#"SELECT id FROM setlists WHERE id = $1;"#)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        tracing::error!("Setlist ID not found.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}
