use crate::{
    errors::api_error::ApiError,
    models::artist::{Artist, CreateArtistPayload, UpdateArtistPayload},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait ArtistRepository: Send + Sync {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Artist>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Artist>, ApiError>;
    async fn create(
        &self,
        payload: &CreateArtistPayload,
        user_id: Uuid,
    ) -> Result<Artist, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateArtistPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(&self, name: &str, user_id: Uuid) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
}

pub struct ArtistRepositoryImpl {
    pub db: PgPool,
}

impl ArtistRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl ArtistRepository for ArtistRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Artist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM artists WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&self.db);

        let artists = sqlx::query_as::<_, Artist>(
            "SELECT id, name, user_id, created_at, updated_at FROM artists WHERE user_id = $1 ORDER BY name ASC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, artists) = tokio::try_join!(count, artists)?;

        Ok((artists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Artist>, ApiError> {
        let artist = sqlx::query_as::<_, Artist>(
            "SELECT id, name, user_id, created_at, updated_at FROM artists WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(artist)
    }

    async fn create(
        &self,
        payload: &CreateArtistPayload,
        user_id: Uuid,
    ) -> Result<Artist, ApiError> {
        let new_artist = Artist::new(&payload.name, user_id);
        sqlx::query(
            "INSERT INTO artists (id, name, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(new_artist.id)
        .bind(&new_artist.name)
        .bind(new_artist.user_id)
        .bind(new_artist.created_at)
        .bind(new_artist.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_artist)
    }

    async fn update(&self, id: Uuid, payload: &UpdateArtistPayload) -> Result<Uuid, ApiError> {
        sqlx::query("UPDATE artists SET name = $1, updated_at = $2 WHERE id = $3")
            .bind(&payload.name)
            .bind(chrono::Utc::now().naive_utc())
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(&self, name: &str, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM artists WHERE name = $1 AND user_id = $2;")
            .bind(name)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some();

        if exists {
            error!("Artist '{name}' already exists for this user.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM artists WHERE id = $1 AND user_id = $2;")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some();

        if !exists {
            error!("Artist ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }
}
