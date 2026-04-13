use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        song::{CreateSongPayload, Song, SongPublic, UpdateSongPayload},
    },
};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SongRepository {
    async fn count(state: &AppState) -> Result<i64, ApiError>;
    async fn find_all(state: &AppState) -> Result<Vec<SongPublic>, ApiError>;
    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<SongPublic>, ApiError>;
    async fn create(
        state: &AppState,
        payload: &CreateSongPayload,
        user_id: Uuid,
    ) -> Result<Song, ApiError>;
    async fn update(state: &AppState, payload: &UpdateSongPayload) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError>;
}

pub struct SongRepositoryImpl;

#[async_trait::async_trait]
impl SongRepository for SongRepositoryImpl {
    async fn count(state: &AppState) -> Result<i64, ApiError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM songs;")
            .fetch_one(&state.db)
            .await?;
        Ok(count)
    }

    async fn find_all(state: &AppState) -> Result<Vec<SongPublic>, ApiError> {
        let songs = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs ORDER BY title ASC"
        )
        .fetch_all(&state.db)
        .await?;
        Ok(songs)
    }

    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<SongPublic>, ApiError> {
        let song = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
        Ok(song)
    }

    async fn create(
        state: &AppState,
        payload: &CreateSongPayload,
        user_id: Uuid,
    ) -> Result<Song, ApiError> {
        let new_song = Song::new(&payload.title, payload.artist_id, Some(user_id));
        sqlx::query("INSERT INTO songs (id, title, artist_id, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(new_song.id)
            .bind(&new_song.title)
            .bind(new_song.artist_id)
            .bind(new_song.user_id)
            .bind(new_song.created_at)
            .bind(new_song.updated_at)
            .execute(&state.db)
            .await?;
        Ok(new_song)
    }

    async fn update(state: &AppState, payload: &UpdateSongPayload) -> Result<Uuid, ApiError> {
        let mut updated = false;

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE songs SET title = $1, updated_at = $2 WHERE id = $3")
                .bind(title)
                .bind(chrono::Utc::now().naive_utc())
                .bind(payload.id)
                .execute(&state.db)
                .await?;
            updated = true;
        }

        if let Some(artist_id) = &payload.artist_id {
            sqlx::query("UPDATE songs SET artist_id = $1, updated_at = $2 WHERE id = $3")
                .bind(artist_id)
                .bind(chrono::Utc::now().naive_utc())
                .bind(payload.id)
                .execute(&state.db)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        Ok(payload.id)
    }

    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM songs WHERE id = $1")
            .bind(payload.id)
            .execute(&state.db)
            .await?;
        Ok(())
    }
}
