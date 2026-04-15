use crate::{database::AppState, errors::api_error::ApiError};
use tracing::error;
use uuid::Uuid;

pub async fn artist_exists(state: &AppState, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM artists WHERE id = $1 AND user_id = $2;")
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        error!("Artist ID not found or unauthorized.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn song_exists(state: &AppState, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query("SELECT id FROM songs WHERE id = $1 AND user_id = $2;")
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        error!("Song ID not found or unauthorized.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}

pub async fn setlist_exists(state: &AppState, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
    let exists = sqlx::query(r#"SELECT id FROM setlists WHERE id = $1 AND user_id = $2;"#)
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .is_some();

    if !exists {
        tracing::error!("Setlist ID not found or unauthorized.");
        Err(ApiError::NotFound)
    } else {
        Ok(())
    }
}
