use crate::{
    errors::api_error::ApiError,
    models::{
        band::BandRole,
        setlist::{
            CreateSetlistPayload, Setlist, SetlistItem, SetlistItemRef, SetlistItemType,
            SetlistMarker, SetlistMarkerType, UpdateSetlistPayload,
        },
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
    /// Creates an independent copy of a setlist the caller can view: title
    /// (optionally overridden), description, and every song/block/break in
    /// it, preserving relative order. The copy always belongs to `user_id`
    /// and is personal (never copies `band_id`), even when duplicating a
    /// band setlist — the caller explicitly re-adds it to a band if wanted.
    async fn duplicate(
        &self,
        id: Uuid,
        user_id: Uuid,
        title_override: Option<String>,
    ) -> Result<Setlist, ApiError>;
    /// Lists a setlist's block/break markers, ordered by position.
    async fn get_markers(&self, setlist_id: Uuid) -> Result<Vec<SetlistMarker>, ApiError>;
    /// The full, position-merged view of a setlist's contents (songs,
    /// block headers, and breaks) used by the setlist builder UI.
    async fn get_items(&self, setlist_id: Uuid) -> Result<Vec<SetlistItem>, ApiError>;
    async fn create_block(&self, setlist_id: Uuid, name: &str) -> Result<SetlistMarker, ApiError>;
    async fn update_block(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        name: &str,
    ) -> Result<SetlistMarker, ApiError>;
    async fn create_break(
        &self,
        setlist_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError>;
    async fn update_break(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError>;
    async fn delete_marker(&self, setlist_id: Uuid, marker_id: Uuid) -> Result<(), ApiError>;
    /// Rewrites the position of every song and marker in `items` to match
    /// its index in the list — songs and markers share one ordering space,
    /// so this is how blocks/breaks get interleaved with songs.
    async fn reorder_items(
        &self,
        setlist_id: Uuid,
        items: &[SetlistItemRef],
    ) -> Result<(), ApiError>;
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
                setlist_total_duration(s.id) AS total_duration
            FROM setlists s
            WHERE s.user_id = $1 AND s.band_id IS NULL
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
                setlist_total_duration(s.id) AS total_duration
            FROM setlists s
            WHERE s.band_id = $1
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
                setlist_total_duration(s.id) AS total_duration
            FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            WHERE s.id = $1 AND (s.user_id = $2 OR bm.user_id IS NOT NULL)
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
                setlist_total_duration(s.id) AS total_duration
            FROM setlists s
            WHERE s.share_token = $1
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

    async fn duplicate(
        &self,
        id: Uuid,
        user_id: Uuid,
        title_override: Option<String>,
    ) -> Result<Setlist, ApiError> {
        let original = self
            .find_by_id(id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        let base_title = title_override
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("{} (copy)", original.title));

        // Read the source markers before opening the write transaction —
        // this is a plain read, same as `original` above.
        let source_markers = self.get_markers(id).await?;

        let mut tx = self.db.begin().await?;

        // Personal copy: try the requested title, then fall back to
        // "<title> (2)", "<title> (3)", ... rather than failing outright
        // on the very common case of duplicating the same setlist twice.
        let mut candidate = base_title.clone();
        let mut suffix = 2;
        let final_title = loop {
            let exists = sqlx::query(
                "SELECT id FROM setlists WHERE title = $1 AND user_id = $2 AND band_id IS NULL;",
            )
            .bind(&candidate)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .is_some();

            if !exists {
                break candidate.clone();
            }

            candidate = format!("{base_title} ({suffix})");
            suffix += 1;

            if suffix > 50 {
                error!(%id, %user_id, "Too many duplicate titles when duplicating setlist.");
                return Err(ApiError::AlreadyExists);
            }
        };

        let new_setlist = Setlist::new(&final_title, original.description.clone(), user_id, None);

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
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO setlist_songs (setlist_id, song_id, position)
            SELECT $2, ss.song_id, ss.position FROM setlist_songs ss WHERE ss.setlist_id = $1
            "#,
        )
        .bind(id)
        .bind(new_setlist.id)
        .execute(&mut *tx)
        .await?;

        for marker in &source_markers {
            sqlx::query(
                "INSERT INTO setlist_markers (id, setlist_id, marker_type, label, duration_minutes, position, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(new_setlist.id)
            .bind(marker.marker_type)
            .bind(&marker.label)
            .bind(marker.duration_minutes)
            .bind(marker.position)
            .bind(new_setlist.created_at)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        // Re-fetch so total_duration comes from the same computation as
        // every other read path, rather than assuming it matches the
        // original (safe today, but fragile to keep in sync by hand).
        self.find_by_id(new_setlist.id, user_id)
            .await?
            .ok_or(ApiError::NotFound)
    }

    async fn get_markers(&self, setlist_id: Uuid) -> Result<Vec<SetlistMarker>, ApiError> {
        let markers = sqlx::query_as::<_, SetlistMarker>(
            "SELECT id, setlist_id, marker_type, label, duration_minutes, position, created_at
             FROM setlist_markers WHERE setlist_id = $1 ORDER BY position ASC;",
        )
        .bind(setlist_id)
        .fetch_all(&self.db)
        .await?;

        Ok(markers)
    }

    async fn get_items(&self, setlist_id: Uuid) -> Result<Vec<SetlistItem>, ApiError> {
        #[derive(sqlx::FromRow)]
        struct SongItemRow {
            position: i32,
            id: Uuid,
            title: String,
            artist_id: Uuid,
            artist_name: String,
            user_id: Uuid,
            band_id: Option<Uuid>,
            forked_from: Option<Uuid>,
            tempo: Option<i32>,
            lyrics: Option<String>,
            tonality: Option<crate::models::song::Tonality>,
            genre: Option<crate::models::song::Genre>,
            duration: Option<i32>,
            created_at: chrono::NaiveDateTime,
            updated_at: chrono::NaiveDateTime,
        }

        let song_rows = async {
            sqlx::query_as::<_, SongItemRow>(
                "SELECT ss.position, s.id, s.title, s.artist_id, a.name AS artist_name, s.user_id, s.band_id, s.forked_from,
                        s.tempo, s.lyrics, s.tonality, s.genre, s.duration, s.created_at, s.updated_at
                 FROM songs s
                 INNER JOIN setlist_songs ss ON s.id = ss.song_id
                 INNER JOIN artists a ON a.id = s.artist_id
                 WHERE ss.setlist_id = $1",
            )
            .bind(setlist_id)
            .fetch_all(&self.db)
            .await
            .map_err(ApiError::from)
        };

        let markers = self.get_markers(setlist_id);

        let (song_rows, markers) = tokio::try_join!(song_rows, markers)?;

        let mut items: Vec<SetlistItem> = song_rows
            .into_iter()
            .map(|r| SetlistItem::Song {
                position: r.position,
                song: crate::models::song::SongWithArtist {
                    id: r.id,
                    title: r.title,
                    artist_id: r.artist_id,
                    artist_name: r.artist_name,
                    user_id: r.user_id,
                    band_id: r.band_id,
                    forked_from: r.forked_from,
                    tempo: r.tempo,
                    lyrics: r.lyrics,
                    tonality: r.tonality,
                    genre: r.genre,
                    duration: r.duration,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                },
            })
            .collect();

        items.extend(markers.into_iter().map(|m| match m.marker_type {
            SetlistMarkerType::Block => SetlistItem::Block {
                position: m.position,
                id: m.id,
                name: m.label.unwrap_or_default(),
            },
            SetlistMarkerType::Break => SetlistItem::Break {
                position: m.position,
                id: m.id,
                label: m.label,
                duration_minutes: m.duration_minutes,
            },
        }));

        items.sort_by_key(|i| match i {
            SetlistItem::Song { position, .. } => *position,
            SetlistItem::Block { position, .. } => *position,
            SetlistItem::Break { position, .. } => *position,
        });

        Ok(items)
    }

    async fn create_block(&self, setlist_id: Uuid, name: &str) -> Result<SetlistMarker, ApiError> {
        self.create_marker(
            setlist_id,
            SetlistMarkerType::Block,
            Some(name.to_string()),
            None,
        )
        .await
    }

    async fn update_block(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        name: &str,
    ) -> Result<SetlistMarker, ApiError> {
        self.update_marker(setlist_id, marker_id, Some(name.to_string()), None, false)
            .await
    }

    async fn create_break(
        &self,
        setlist_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError> {
        self.create_marker(
            setlist_id,
            SetlistMarkerType::Break,
            label,
            duration_minutes,
        )
        .await
    }

    async fn update_break(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError> {
        self.update_marker(setlist_id, marker_id, label, duration_minutes, true)
            .await
    }

    async fn delete_marker(&self, setlist_id: Uuid, marker_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM setlist_markers WHERE id = $1 AND setlist_id = $2;")
            .bind(marker_id)
            .bind(setlist_id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn reorder_items(
        &self,
        setlist_id: Uuid,
        items: &[SetlistItemRef],
    ) -> Result<(), ApiError> {
        let song_ids: Vec<Uuid> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.item_type == SetlistItemType::Song)
            .map(|(_, i)| i.id)
            .collect();
        let song_positions: Vec<i32> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.item_type == SetlistItemType::Song)
            .map(|(idx, _)| idx as i32 + 1)
            .collect();

        let marker_ids: Vec<Uuid> = items
            .iter()
            .filter(|i| i.item_type != SetlistItemType::Song)
            .map(|i| i.id)
            .collect();
        let marker_positions: Vec<i32> = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.item_type != SetlistItemType::Song)
            .map(|(idx, _)| idx as i32 + 1)
            .collect();

        let mut tx = self.db.begin().await?;

        if !song_ids.is_empty() {
            sqlx::query(
                r#"
                UPDATE setlist_songs AS ss
                SET position = u.new_position
                FROM (
                    SELECT unnest($1::uuid[]) AS id, unnest($2::int[]) AS new_position
                ) AS u
                WHERE ss.setlist_id = $3 AND ss.song_id = u.id
                "#,
            )
            .bind(&song_ids)
            .bind(&song_positions)
            .bind(setlist_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("Failed to reorder setlist songs: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        if !marker_ids.is_empty() {
            sqlx::query(
                r#"
                UPDATE setlist_markers AS sm
                SET position = u.new_position
                FROM (
                    SELECT unnest($1::uuid[]) AS id, unnest($2::int[]) AS new_position
                ) AS u
                WHERE sm.setlist_id = $3 AND sm.id = u.id
                "#,
            )
            .bind(&marker_ids)
            .bind(&marker_positions)
            .bind(setlist_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                tracing::error!("Failed to reorder setlist markers: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        tx.commit().await?;

        Ok(())
    }
}

impl SetlistRepositoryImpl {
    async fn create_marker(
        &self,
        setlist_id: Uuid,
        marker_type: SetlistMarkerType,
        label: Option<String>,
        duration_minutes: Option<i32>,
    ) -> Result<SetlistMarker, ApiError> {
        let id = Uuid::new_v4();
        let now = chrono::Utc::now().naive_utc();

        // Append at the end of the shared song/marker ordering space.
        let next_position: i32 = sqlx::query_scalar(
            r#"
            SELECT COALESCE(GREATEST(
                (SELECT MAX(position) FROM setlist_songs WHERE setlist_id = $1),
                (SELECT MAX(position) FROM setlist_markers WHERE setlist_id = $1)
            ), 0) + 1
            "#,
        )
        .bind(setlist_id)
        .fetch_one(&self.db)
        .await?;

        sqlx::query(
            "INSERT INTO setlist_markers (id, setlist_id, marker_type, label, duration_minutes, position, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(setlist_id)
        .bind(marker_type)
        .bind(&label)
        .bind(duration_minutes)
        .bind(next_position)
        .bind(now)
        .execute(&self.db)
        .await?;

        Ok(SetlistMarker {
            id,
            setlist_id,
            marker_type,
            label,
            duration_minutes,
            position: next_position,
            created_at: now,
        })
    }

    async fn update_marker(
        &self,
        setlist_id: Uuid,
        marker_id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
        allow_null_duration: bool,
    ) -> Result<SetlistMarker, ApiError> {
        let result = if allow_null_duration {
            sqlx::query(
                "UPDATE setlist_markers SET label = $1, duration_minutes = $2
                 WHERE id = $3 AND setlist_id = $4",
            )
            .bind(&label)
            .bind(duration_minutes)
            .bind(marker_id)
            .bind(setlist_id)
            .execute(&self.db)
            .await?
        } else {
            sqlx::query("UPDATE setlist_markers SET label = $1 WHERE id = $2 AND setlist_id = $3")
                .bind(&label)
                .bind(marker_id)
                .bind(setlist_id)
                .execute(&self.db)
                .await?
        };

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        let marker = sqlx::query_as::<_, SetlistMarker>(
            "SELECT id, setlist_id, marker_type, label, duration_minutes, position, created_at
             FROM setlist_markers WHERE id = $1;",
        )
        .bind(marker_id)
        .fetch_one(&self.db)
        .await?;

        Ok(marker)
    }
}
