use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::song::{CreateSongPayload, Song, SongPublic, UpdateSongPayload},
};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SongRepository {
    async fn find_all(
        state: &AppState,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongPublic>, i64), ApiError>;
    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SongPublic>, ApiError>;
    async fn create(
        state: &AppState,
        payload: &CreateSongPayload,
        user_id: Uuid,
    ) -> Result<Song, ApiError>;
    async fn update(
        state: &AppState,
        id: Uuid,
        payload: &UpdateSongPayload,
    ) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, id: Uuid) -> Result<(), ApiError>;
}

pub struct SongRepositoryImpl;

#[async_trait::async_trait]
impl SongRepository for SongRepositoryImpl {
    async fn find_all(
        state: &AppState,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongPublic>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM songs WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&state.db);

        let songs = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs WHERE user_id = $1 ORDER BY title ASC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&state.db);

        let (count, songs) = tokio::try_join!(count, songs)?;

        Ok((songs, count))
    }

    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SongPublic>, ApiError> {
        let song = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
        Ok(song)
    }

    async fn create(
        state: &AppState,
        payload: &CreateSongPayload,
        user_id: Uuid,
    ) -> Result<Song, ApiError> {
        let new_song = Song::new(&payload.title, payload.artist_id, user_id);
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

    async fn update(
        state: &AppState,
        id: Uuid,
        payload: &UpdateSongPayload,
    ) -> Result<Uuid, ApiError> {
        let mut updated = false;
        let now = chrono::Utc::now().naive_utc();

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE songs SET title = $1, updated_at = $2 WHERE id = $3")
                .bind(title)
                .bind(now)
                .bind(id)
                .execute(&state.db)
                .await?;
            updated = true;
        }

        if let Some(artist_id) = &payload.artist_id {
            sqlx::query("UPDATE songs SET artist_id = $1, updated_at = $2 WHERE id = $3")
                .bind(artist_id)
                .bind(now)
                .bind(id)
                .execute(&state.db)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        Ok(id)
    }

    async fn delete(state: &AppState, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM songs WHERE id = $1")
            .bind(id)
            .execute(&state.db)
            .await?;
        Ok(())
    }
}
