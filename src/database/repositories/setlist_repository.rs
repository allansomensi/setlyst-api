use crate::{
    errors::api_error::ApiError,
    models::{
        band::BandRole,
        setlist::{CreateSetlistPayload, Setlist, UpdateSetlistPayload},
    },
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SetlistRepository: Send + Sync {
    /// Lists the caller's personal setlists (i.e. `band_id IS NULL`).
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Lists every setlist that belongs to a band. Callers must check band
    /// membership themselves before calling this.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Fetches a setlist the caller may *view*: either their own personal
    /// setlist, or a setlist belonging to any band they are a member of.
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError>;
    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        band_id: Option<Uuid>,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    /// Checks the caller may *view* the setlist (see `find_by_id`).
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may *manage* (edit/delete/add or remove songs from)
    /// the setlist: its personal owner, or a band member whose role clears
    /// the band's setlist-management bar (`moderator`+, or `member` when the
    /// band allows it).
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Generates a fresh public share token for the setlist, replacing any
    /// existing one (which immediately invalidates previously shared links).
    /// Returns the updated setlist.
    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError>;
    /// Disables public sharing (clears the share token).
    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Resolves a setlist by its public share token. No ownership or
    /// membership filter — this is the lookup used by the unauthenticated
    /// `/public/setlists/{token}` routes.
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Setlist>, ApiError>;
    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        position: i32,
    ) -> Result<(), ApiError>;
    /// Checks whether a song is already part of a setlist — used to reject
    /// adding the same song twice rather than silently repositioning it.
    async fn has_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<bool, ApiError>;
    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError>;
    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<crate::models::song::SongWithArtist>, i64), ApiError>;
    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError>;
}

pub struct SetlistRepositoryImpl {
    pub db: PgPool,
}

impl SetlistRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// Row shape used to decide write permission on a setlist without fetching
/// its full column set.
#[derive(sqlx::FromRow)]
struct SetlistAccessRow {
    owner_id: Uuid,
    band_id: Option<Uuid>,
    band_role: Option<BandRole>,
    members_can_manage_setlists: Option<bool>,
}

#[async_trait::async_trait]
impl SetlistRepository for SetlistRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL;",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(
            r#"
            SELECT
                s.id, s.title, s.description, s.user_id, s.band_id, s.share_token, s.created_at, s.updated_at,
                COALESCE(SUM(so.duration), 0)::integer AS total_duration
            FROM setlists s
            LEFT JOIN setlist_songs ss ON s.id = ss.setlist_id
            LEFT JOIN songs so ON ss.song_id = so.id
            WHERE s.user_id = $1 AND s.band_id IS NULL
            GROUP BY s.id
            ORDER BY s.title ASC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM setlists WHERE band_id = $1;")
            .bind(band_id)
            .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(
            r#"
            SELECT
                s.id, s.title, s.description, s.user_id, s.band_id, s.share_token, s.created_at, s.updated_at,
                COALESCE(SUM(so.duration), 0)::integer AS total_duration
            FROM setlists s
            LEFT JOIN setlist_songs ss ON s.id = ss.setlist_id
            LEFT JOIN songs so ON ss.song_id = so.id
            WHERE s.band_id = $1
            GROUP BY s.id
            ORDER BY s.title ASC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(band_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(
            r#"
            SELECT
                s.id, s.title, s.description, s.user_id, s.band_id, s.share_token, s.created_at, s.updated_at,
                COALESCE(SUM(so.duration), 0)::integer AS total_duration
            FROM setlists s
            LEFT JOIN setlist_songs ss ON s.id = ss.setlist_id
            LEFT JOIN songs so ON ss.song_id = so.id
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            WHERE s.id = $1 AND (s.user_id = $2 OR bm.user_id IS NOT NULL)
            GROUP BY s.id
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError> {
        let new_setlist = Setlist::new(
            &payload.title,
            payload.description.clone(),
            user_id,
            payload.band_id,
        );
        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(new_setlist.band_id)
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_setlist)
    }

    async fn update(&self, id: Uuid, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE setlists SET title = $1 WHERE id = $2")
                .bind(title)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE setlists SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE setlists SET updated_at = $1 WHERE id = $2")
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
        sqlx::query("DELETE FROM setlists WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        band_id: Option<Uuid>,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let exists = match (band_id, exclude_id) {
            (Some(band_id), Some(id)) => sqlx::query(
                "SELECT id FROM setlists WHERE title = $1 AND band_id = $2 AND id != $3;",
            )
            .bind(title)
            .bind(band_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
            (Some(band_id), None) => {
                sqlx::query("SELECT id FROM setlists WHERE title = $1 AND band_id = $2;")
                    .bind(title)
                    .bind(band_id)
                    .fetch_optional(&self.db)
                    .await?
                    .is_some()
            }
            (None, Some(id)) => sqlx::query(
                "SELECT id FROM setlists WHERE title = $1 AND user_id = $2 AND band_id IS NULL AND id != $3;",
            )
            .bind(title)
            .bind(user_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
            (None, None) => sqlx::query(
                "SELECT id FROM setlists WHERE title = $1 AND user_id = $2 AND band_id IS NULL;",
            )
            .bind(title)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
        };

        if exists {
            error!("Setlist '{title}' already exists in this scope.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query(
            r#"
            SELECT s.id FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            WHERE s.id = $1 AND (s.user_id = $2 OR bm.user_id IS NOT NULL);
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if !exists {
            error!("Setlist ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let row = sqlx::query_as::<_, SetlistAccessRow>(
            r#"
            SELECT
                s.user_id AS owner_id,
                s.band_id,
                bm.role AS band_role,
                b.members_can_manage_setlists
            FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            LEFT JOIN bands b ON b.id = s.band_id
            WHERE s.id = $1
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        let allowed = match row.band_id {
            None => row.owner_id == user_id,
            Some(_) => match row.band_role {
                Some(role) if role.satisfies(BandRole::Moderator) => true,
                Some(BandRole::Member) => row.members_can_manage_setlists.unwrap_or(false),
                _ => false,
            },
        };

        if allowed {
            Ok(())
        } else {
            error!(%id, %user_id, "User is not allowed to manage this setlist.");
            Err(ApiError::Forbidden)
        }
    }

    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError> {
        let token = crate::utils::share_token::generate_share_token();

        let result = sqlx::query("UPDATE setlists SET share_token = $1 WHERE id = $2")
            .bind(&token)
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        // Re-fetch through the same query used by the public routes so the
        // returned `total_duration` is computed consistently, rather than
        // duplicating that aggregation here.
        self.find_by_share_token(&token)
            .await?
            .ok_or(ApiError::NotFound)
    }

    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("UPDATE setlists SET share_token = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn find_by_share_token(&self, token: &str) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(
            r#"
            SELECT
                s.id, s.title, s.description, s.user_id, s.band_id, s.share_token, s.created_at, s.updated_at,
                COALESCE(SUM(so.duration), 0)::integer AS total_duration
            FROM setlists s
            LEFT JOIN setlist_songs ss ON s.id = ss.setlist_id
            LEFT JOIN songs so ON ss.song_id = so.id
            WHERE s.share_token = $1
            GROUP BY s.id
            "#,
        )
        .bind(token)
        .fetch_optional(&self.db)
        .await?;

        Ok(setlist)
    }

    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        position: i32,
    ) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)
             ON CONFLICT (setlist_id, song_id) DO UPDATE SET position = $3;",
        )
        .bind(setlist_id)
        .bind(song_id)
        .bind(position)
        .execute(&self.db)
        .await
        .map_err(|e| {
            tracing::error!("Error adding song to setlist: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(())
    }

    async fn has_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<bool, ApiError> {
        let exists =
            sqlx::query("SELECT 1 FROM setlist_songs WHERE setlist_id = $1 AND song_id = $2;")
                .bind(setlist_id)
                .bind(song_id)
                .fetch_optional(&self.db)
                .await?
                .is_some();

        Ok(exists)
    }

    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError> {
        let result =
            sqlx::query("DELETE FROM setlist_songs WHERE setlist_id = $1 AND song_id = $2;")
                .bind(setlist_id)
                .bind(song_id)
                .execute(&self.db)
                .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<crate::models::song::SongWithArtist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM setlist_songs WHERE setlist_id = $1;")
            .bind(setlist_id)
            .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, crate::models::song::SongWithArtist>(
            "SELECT s.id, s.title, s.artist_id, a.name AS artist_name, s.user_id, s.band_id, s.forked_from,
                    s.tempo, s.lyrics, s.tonality, s.genre, s.duration, s.created_at, s.updated_at
             FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE ss.setlist_id = $1
             ORDER BY ss.position ASC
             LIMIT $2 OFFSET $3;",
        )
        .bind(setlist_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError> {
        sqlx::query(
            r#"
            UPDATE setlist_songs AS ss
            SET position = u.new_position
            FROM (
                SELECT unnest($1::uuid[]) AS id,
                       generate_series(1, array_length($1::uuid[], 1)) AS new_position
            ) AS u
            WHERE ss.setlist_id = $2 AND ss.song_id = u.id
            "#,
        )
        .bind(song_ids)
        .bind(setlist_id)
        .execute(&self.db)
        .await
        .map_err(|e| {
            tracing::error!("Failed to reorder setlist songs: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(())
    }
}
