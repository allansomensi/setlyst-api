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
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateArtistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes an artist (and, through the foreign key, its songs).
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Moves a live artist and its live songs to the trash, as one batch
    /// (restored together). Returns how many songs went with it.
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<i64, ApiError>;
    /// Checks name uniqueness among the caller's *personal* artists.
    async fn is_unique(
        &self,
        name: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// The caller's live personal artist with this name (case-insensitive).
    async fn find_personal_by_name(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Option<Uuid>, ApiError>;
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
            "SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL;",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let artists = sqlx::query_as::<_, Artist>(
            "SELECT a.id, a.name, a.user_id, a.band_id, a.forked_from, a.updated_by,
                    (SELECT u.username FROM users u WHERE u.id = a.updated_by) AS updated_by_username,
                    (SELECT COUNT(*) FROM songs s WHERE s.artist_id = a.id AND s.deleted_at IS NULL) AS song_count,
                    a.created_at, a.updated_at
             FROM artists a
             WHERE a.user_id = $1 AND a.band_id IS NULL AND a.deleted_at IS NULL
             ORDER BY LOWER(a.name) ASC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, artists) = tokio::try_join!(count, artists)?;

        Ok((artists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Artist>, ApiError> {
        // Personal artists are only visible to their owner. Band-owned
        // artists are visible (read-only) to any member of that band, like
        // the band's songs.
        let artist = sqlx::query_as::<_, Artist>(
            "SELECT a.id, a.name, a.user_id, a.band_id, a.forked_from, a.updated_by,
                    (SELECT u.username FROM users u WHERE u.id = a.updated_by) AS updated_by_username,
                    (SELECT COUNT(*) FROM songs s WHERE s.artist_id = a.id AND s.deleted_at IS NULL) AS song_count,
                    a.created_at, a.updated_at
             FROM artists a
             WHERE a.id = $1 AND a.deleted_at IS NULL AND (
                (a.band_id IS NULL AND a.user_id = $2)
                OR (a.band_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM band_members bm WHERE bm.band_id = a.band_id AND bm.user_id = $2
                ))
             )",
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
        let new_artist = Artist::new(payload.name.trim(), user_id);
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

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateArtistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let Some(name) = &payload.name else {
            return Err(ApiError::NotModified);
        };

        let result = sqlx::query(
            "UPDATE artists SET name = $1, updated_at = $2, updated_by = $3 WHERE id = $4",
        )
        .bind(name.trim())
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<i64, ApiError> {
        let now = chrono::Utc::now().naive_utc();
        let batch = Uuid::new_v4();
        let mut tx = self.db.begin().await?;

        let result = sqlx::query(
            "UPDATE artists SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(now)
        .bind(actor_id)
        .bind(batch)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        let songs = sqlx::query(
            "UPDATE songs SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE artist_id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(now)
        .bind(actor_id)
        .bind(batch)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(songs.rows_affected() as i64)
    }

    async fn is_unique(
        &self,
        name: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let exists = match exclude_id {
            Some(id) => sqlx::query(
                "SELECT id FROM artists WHERE LOWER(name) = LOWER($1) AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL AND id != $3;",
            )
            .bind(name)
            .bind(user_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
            None => sqlx::query(
                "SELECT id FROM artists WHERE LOWER(name) = LOWER($1) AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL;",
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
        // Personal artists only: a band-owned artist this user happened to
        // fork must not be attachable to their personal songs.
        let exists = sqlx::query(
            "SELECT id FROM artists WHERE id = $1 AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL;",
        )
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

    async fn find_personal_by_name(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Option<Uuid>, ApiError> {
        let id = sqlx::query_scalar(
            "SELECT id FROM artists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND LOWER(name) = LOWER($2)
             ORDER BY created_at LIMIT 1",
        )
        .bind(user_id)
        .bind(name.trim())
        .fetch_optional(&self.db)
        .await?;
        Ok(id)
    }

    async fn find_or_create_for_band(
        &self,
        band_id: Uuid,
        name: &str,
        creator_id: Uuid,
        source_artist_id: Option<Uuid>,
    ) -> Result<Artist, ApiError> {
        let find = || {
            sqlx::query_as::<_, Artist>(
                "SELECT id, name, user_id, band_id, forked_from, created_at, updated_at FROM artists
                 WHERE band_id = $1 AND LOWER(name) = LOWER($2) AND deleted_at IS NULL",
            )
            .bind(band_id)
            .bind(name)
            .fetch_optional(&self.db)
        };
        if let Some(existing) = find().await? {
            return Ok(existing);
        }

        let new_artist = Artist::new_for_band(name, band_id, creator_id, source_artist_id);
        let inserted = sqlx::query(
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
        .await;

        match inserted {
            Ok(_) => Ok(new_artist),
            // A concurrent copy into the same band (two suggestions
            // accepted at once) created it first: use that one.
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
                find().await?.ok_or(ApiError::AlreadyExists)
            }
            Err(e) => Err(e.into()),
        }
    }
}
