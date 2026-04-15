use crate::{
    errors::api_error::ApiError,
    models::song::{CreateSongPayload, Song, SongPublic, UpdateSongPayload},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SongRepository: Send + Sync {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongPublic>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<SongPublic>, ApiError>;
    async fn create(&self, payload: &CreateSongPayload, user_id: Uuid) -> Result<Song, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateSongPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(&self, title: &str, artist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
}

pub struct SongRepositoryImpl {
    pub db: PgPool,
}

impl SongRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl SongRepository for SongRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongPublic>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM songs WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs WHERE user_id = $1 ORDER BY title ASC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<SongPublic>, ApiError> {
        let song = sqlx::query_as::<_, SongPublic>(
            "SELECT id, title, artist_id, user_id, created_at, updated_at FROM songs WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(song)
    }

    async fn create(&self, payload: &CreateSongPayload, user_id: Uuid) -> Result<Song, ApiError> {
        let new_song = Song::new(&payload.title, payload.artist_id, user_id);
        sqlx::query(
            "INSERT INTO songs (id, title, artist_id, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(new_song.id)
        .bind(&new_song.title)
        .bind(new_song.artist_id)
        .bind(new_song.user_id)
        .bind(new_song.created_at)
        .bind(new_song.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_song)
    }

    async fn update(&self, id: Uuid, payload: &UpdateSongPayload) -> Result<Uuid, ApiError> {
        sqlx::query("UPDATE songs SET title = $1, artist_id = $2, updated_at = $3 WHERE id = $4")
            .bind(&payload.title)
            .bind(payload.artist_id)
            .bind(chrono::Utc::now().naive_utc())
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM songs WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(&self, title: &str, artist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query(
            "SELECT id FROM songs WHERE title = $1 AND artist_id = $2 AND user_id = $3;",
        )
        .bind(title)
        .bind(artist_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if exists {
            error!("Song '{title}' already exists for this artist.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM songs WHERE id = $1 AND user_id = $2;")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some();

        if !exists {
            error!("Song ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }
}
