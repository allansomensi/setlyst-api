use crate::{
    errors::api_error::ApiError,
    models::artist::{Artist, CreateArtistPayload, UpdateArtistPayload},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait ArtistRepository: Send + Sync {
    /// Lists the caller's personal artists (i.e. `band_id IS NULL`).
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
    /// Checks name uniqueness among the caller's *personal* artists.
    async fn is_unique(
        &self,
        name: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Finds an existing band-owned artist with this name (case-insensitive),
    /// or creates one. Used when forking a song into a band — see
    /// [`crate::models::song::Song::fork_for_band`] — so every member's
    /// contribution of, say, "The Beatles" resolves to the same band artist
    /// instead of piling up duplicates. `source_artist_id`, when given, is
    /// recorded as `forked_from` on a newly created band artist, so
    /// platform-wide metrics can tell it apart from genuinely new content.
    async fn find_or_create_for_band(
        &self,
        band_id: Uuid,
        name: &str,
        creator_id: Uuid,
        source_artist_id: Option<Uuid>,
    ) -> Result<Artist, ApiError>;
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

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL;",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let artists = sqlx::query_as::<_, Artist>(
            "SELECT id, name, user_id, band_id, forked_from, created_at, updated_at FROM artists
             WHERE user_id = $1 AND band_id IS NULL
             ORDER BY name ASC LIMIT $2 OFFSET $3",
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
            "SELECT id, name, user_id, band_id, forked_from, created_at, updated_at FROM artists WHERE id = $1 AND user_id = $2",
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
            "INSERT INTO artists (id, name, user_id, band_id, forked_from, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(new_artist.id)
        .bind(&new_artist.name)
        .bind(new_artist.user_id)
        .bind(new_artist.band_id)
        .bind(new_artist.forked_from)
        .bind(new_artist.created_at)
        .bind(new_artist.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_artist)
    }

    async fn update(&self, id: Uuid, payload: &UpdateArtistPayload) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(name) = &payload.name {
            sqlx::query("UPDATE artists SET name = $1 WHERE id = $2")
                .bind(name)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE artists SET updated_at = $1 WHERE id = $2")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(id)
        } else {
            Err(ApiError::NotModified)
        }
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(
        &self,
        name: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let exists = match exclude_id {
            Some(id) => sqlx::query(
                "SELECT id FROM artists WHERE name = $1 AND user_id = $2 AND band_id IS NULL AND id != $3;",
            )
            .bind(name)
            .bind(user_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
            None => sqlx::query(
                "SELECT id FROM artists WHERE name = $1 AND user_id = $2 AND band_id IS NULL;",
            )
            .bind(name)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
        };

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

    async fn find_or_create_for_band(
        &self,
        band_id: Uuid,
        name: &str,
        creator_id: Uuid,
        source_artist_id: Option<Uuid>,
    ) -> Result<Artist, ApiError> {
        if let Some(existing) = sqlx::query_as::<_, Artist>(
            "SELECT id, name, user_id, band_id, forked_from, created_at, updated_at FROM artists
             WHERE band_id = $1 AND LOWER(name) = LOWER($2)",
        )
        .bind(band_id)
        .bind(name)
        .fetch_optional(&self.db)
        .await?
        {
            return Ok(existing);
        }

        let new_artist = Artist::new_for_band(name, band_id, creator_id, source_artist_id);
        sqlx::query(
            "INSERT INTO artists (id, name, user_id, band_id, forked_from, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(new_artist.id)
        .bind(&new_artist.name)
        .bind(new_artist.user_id)
        .bind(new_artist.band_id)
        .bind(new_artist.forked_from)
        .bind(new_artist.created_at)
        .bind(new_artist.updated_at)
        .execute(&self.db)
        .await?;

        Ok(new_artist)
    }
}
