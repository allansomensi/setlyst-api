use crate::database::repositories::song_repository::{like_pattern, song_columns};
use crate::{
    errors::api_error::{ApiError, codes},
    models::{
        band::BandRole,
        link::Links,
        quota::QuotaLimits,
        setlist::{
            CreateSetlistPayload, Setlist, SetlistItem, SetlistItemRef, SetlistItemType,
            SetlistMarker, SetlistMarkerType, UpdateSetlistPayload,
        },
        song::{Song, SongWithArtist},
    },
    validations::link::normalize_links,
};
use axum::http::StatusCode;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`Setlist`] from `setlists s` (everything but the
/// caller-specific `is_favorite`). A macro so it can be spliced into
/// `concat!` — sqlx only accepts literal query strings. `song_count` only
/// counts live songs of the setlist's own scope (see [`scoped_songs!`]).
macro_rules! setlist_columns {
    () => {
        "s.id, s.title, s.description, s.user_id, s.band_id, s.share_token,
         s.share_locked_at, s.share_lock_reason, s.created_at, s.updated_at, s.updated_by,
         s.links, s.is_repertoire,
         (SELECT u.username FROM users u WHERE u.id = s.updated_by) AS updated_by_username,
         (SELECT u.username FROM users u WHERE u.id = s.user_id) AS owner_username,
         (SELECT COUNT(*) FROM setlist_songs sc
            INNER JOIN songs so ON so.id = sc.song_id
            WHERE sc.setlist_id = s.id AND so.deleted_at IS NULL
              AND ((s.band_id IS NULL AND so.band_id IS NULL AND so.user_id = s.user_id)
                   OR so.band_id = s.band_id)) AS song_count,
         setlist_total_duration(s.id) AS total_duration"
    };
}

/// The condition keeping a setlist's songs to live songs of its own scope:
/// a personal setlist only shows its owner's personal songs and a band
/// setlist only that band's copies — even if a stale row points elsewhere
/// (defense in depth: an old duplicate could hold band songs in a personal
/// setlist). Expects `songs s` and `setlists st`.
macro_rules! scoped_songs {
    () => {
        "s.deleted_at IS NULL
         AND ((st.band_id IS NULL AND s.band_id IS NULL AND s.user_id = st.user_id)
              OR s.band_id = st.band_id)"
    };
}

#[async_trait::async_trait]
pub trait SetlistRepository: Send + Sync {
    /// Lists the caller's personal setlists (i.e. `band_id IS NULL`).
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Lists every live setlist of a band, the repertoire first. Callers
    /// must check band membership themselves before calling this.
    /// `user_id` is only used to resolve each setlist's `is_favorite`.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    /// Fetches a live setlist the caller may *view*: either their own
    /// personal setlist, or a setlist belonging to any band they are a
    /// member of.
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError>;
    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError>;
    /// Title changes on a repertoire fail with `REPERTOIRE_PROTECTED`.
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSetlistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes a setlist (staff tooling and the trash). A
    /// repertoire can't be deleted (`REPERTOIRE_PROTECTED`).
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Moves a live setlist to the trash (`REPERTOIRE_PROTECTED` for a
    /// repertoire).
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Records that the setlist's contents changed (songs, blocks, breaks,
    /// order) — bumps `updated_at` and `updated_by`, so "last modified"
    /// reflects the running order, not just the title.
    async fn touch(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Any live setlist by ID, without an access filter (staff tooling and
    /// public gig pages).
    async fn find_any(&self, id: Uuid) -> Result<Option<Setlist>, ApiError>;
    /// Staff takedown: clears the share token and locks sharing so the
    /// owner can't re-enable it until staff unlocks it. Gigs showing this
    /// setlist lose their public link too.
    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Marks a setlist as a favorite for this user. Idempotent — favoriting
    /// an already-favorited setlist is a no-op, not an error.
    async fn add_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Un-favorites a setlist for this user. Idempotent.
    async fn remove_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Title uniqueness among the live setlists of the scope (the
    /// repertoire never counts).
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
    /// Checks the caller may export this setlist to PDF: its personal
    /// owner, or a band member whose role satisfies the band's
    /// `export_pdf` permission (`admin`+ always can; `moderator`/`member`
    /// follow the band's configurable permission matrix).
    async fn can_export_pdf(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Generates a fresh public share token for the setlist, replacing any
    /// existing one (which immediately invalidates previously shared links).
    /// Returns the updated setlist.
    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError>;
    /// Disables public sharing (clears the share token).
    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Resolves a live setlist by its public share token. No ownership or
    /// membership filter — this is the lookup used by the unauthenticated
    /// `/public/setlists/{token}` routes.
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Setlist>, ApiError>;
    /// Adds a song to the end of the setlist's shared song/marker ordering
    /// space (the position is always computed server-side). For a band
    /// setlist the song is also appended to the band's repertoire when it
    /// isn't there yet, in the same transaction.
    async fn add_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError>;
    /// Checks whether a song is already part of a setlist — used to reject
    /// adding the same song twice rather than silently repositioning it.
    async fn has_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<bool, ApiError>;
    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError>;
    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError>;
    /// Live songs with their position in the running order (public pages).
    async fn get_positioned_songs(
        &self,
        setlist_id: Uuid,
    ) -> Result<Vec<(i32, SongWithArtist)>, ApiError>;
    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError>;
    /// Creates an independent personal copy of a setlist the caller can
    /// view: title (optionally overridden), description, links and every
    /// song/block/break in it, preserving relative order.
    ///
    /// Band songs are never referenced from the copy: each is replaced by a
    /// personal copy in the caller's library (an existing personal song
    /// with the same title and artist is reused; otherwise the artist and
    /// song are created). Songs that would take the caller over their song
    /// or artist quota are left out; their number is returned alongside
    /// the new setlist.
    async fn duplicate(
        &self,
        id: Uuid,
        user_id: Uuid,
        title_override: Option<String>,
        limits: Option<QuotaLimits>,
    ) -> Result<(Setlist, i64), ApiError>;
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
    /// The id of a band's repertoire.
    async fn find_repertoire_id(&self, band_id: Uuid) -> Result<Option<Uuid>, ApiError>;
    /// Live songs of a band's repertoire, alphabetically, optionally
    /// filtered by title or artist.
    async fn get_repertoire_songs(
        &self,
        band_id: Uuid,
        search: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError>;
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
    role_permission_allowed: Option<bool>,
}

pub(crate) fn repertoire_protected() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::REPERTOIRE_PROTECTED,
        "The band's repertoire can't be deleted or renamed.",
    )
}

/// Appends `song_id` at the end of `setlist_id` (no-op if already there).
async fn append_song(
    tx: &mut Transaction<'_, Postgres>,
    setlist_id: Uuid,
    song_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        INSERT INTO setlist_songs (setlist_id, song_id, position)
        SELECT $1, $2, COALESCE(GREATEST(
            (SELECT MAX(position) FROM setlist_songs WHERE setlist_id = $1),
            (SELECT MAX(position) FROM setlist_markers WHERE setlist_id = $1)
        ), 0) + 1
        ON CONFLICT (setlist_id, song_id) DO NOTHING
        "#,
    )
    .bind(setlist_id)
    .bind(song_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
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
            "SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL;",
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $1) AS is_favorite
            FROM setlists s
            WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
            ORDER BY is_favorite DESC, LOWER(s.title) ASC, s.id ASC
            LIMIT $2 OFFSET $3"
        ))
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
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlists WHERE band_id = $1 AND deleted_at IS NULL;",
        )
        .bind(band_id)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $4) AS is_favorite
            FROM setlists s
            WHERE s.band_id = $1 AND s.deleted_at IS NULL
            ORDER BY s.is_repertoire DESC, is_favorite DESC, LOWER(s.title) ASC, s.id ASC
            LIMIT $2 OFFSET $3"
        ))
        .bind(band_id)
        .bind(size)
        .bind(offset)
        .bind(user_id)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", EXISTS(SELECT 1 FROM favorite_setlists f WHERE f.setlist_id = s.id AND f.user_id = $2) AS is_favorite
            FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            WHERE s.id = $1 AND s.deleted_at IS NULL
              AND ((s.band_id IS NULL AND s.user_id = $2) OR bm.user_id IS NOT NULL)"
        ))
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
        let links = normalize_links(payload.links.as_deref().unwrap_or_default())?;
        let description = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        let mut new_setlist =
            Setlist::new(payload.title.trim(), description, user_id, payload.band_id);
        new_setlist.links = Links::from_stored(links.clone());

        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(new_setlist.band_id)
        .bind(sqlx::types::Json(&links))
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_setlist)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSetlistPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        let is_repertoire: bool = sqlx::query_scalar(
            "SELECT is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;

        if let Some(title) = &payload.title {
            if is_repertoire {
                return Err(repertoire_protected());
            }
            sqlx::query("UPDATE setlists SET title = $1 WHERE id = $2")
                .bind(title.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            let description = description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty());
            sqlx::query("UPDATE setlists SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(links) = &payload.links {
            let links = normalize_links(links)?;
            sqlx::query("UPDATE setlists SET links = $1 WHERE id = $2")
                .bind(sqlx::types::Json(&links))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE setlists SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let is_repertoire: Option<bool> =
            sqlx::query_scalar("SELECT is_repertoire FROM setlists WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await?;
        match is_repertoire {
            None => return Err(ApiError::NotFound),
            Some(true) => return Err(repertoire_protected()),
            Some(false) => {}
        }
        let result = sqlx::query("DELETE FROM setlists WHERE id = $1 AND NOT is_repertoire")
            .bind(id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let is_repertoire: Option<bool> = sqlx::query_scalar(
            "SELECT is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match is_repertoire {
            None => return Err(ApiError::NotFound),
            Some(true) => return Err(repertoire_protected()),
            Some(false) => {}
        }
        let result = sqlx::query(
            "UPDATE setlists SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL AND NOT is_repertoire",
        )
        .bind(id)
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(Uuid::new_v4())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn touch(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("UPDATE setlists SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", false AS is_favorite FROM setlists s WHERE s.id = $1 AND s.deleted_at IS NULL"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let result = sqlx::query(
            "UPDATE setlists
             SET share_token = NULL, share_locked_at = $1, share_locked_by = $2, share_lock_reason = $3
             WHERE id = $4",
        )
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        // A public gig page would otherwise keep showing the setlist that
        // was just taken down.
        sqlx::query("UPDATE gigs SET share_token = NULL WHERE setlist_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE setlists
             SET share_locked_at = NULL, share_locked_by = NULL, share_lock_reason = NULL
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn add_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO favorite_setlists (user_id, setlist_id, created_at) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, setlist_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(setlist_id)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn remove_favorite(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM favorite_setlists WHERE user_id = $1 AND setlist_id = $2")
            .bind(user_id)
            .bind(setlist_id)
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
        let exists = sqlx::query(
            "SELECT id FROM setlists
             WHERE title = $1 AND deleted_at IS NULL AND NOT is_repertoire
               AND (($2::uuid IS NOT NULL AND band_id = $2)
                    OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $3))
               AND ($4::uuid IS NULL OR id != $4)",
        )
        .bind(title.trim())
        .bind(band_id)
        .bind(user_id)
        .bind(exclude_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

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
            WHERE s.id = $1 AND s.deleted_at IS NULL
              AND ((s.band_id IS NULL AND s.user_id = $2) OR bm.user_id IS NOT NULL);
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
        self.check_permission(id, user_id, "manage_setlists").await
    }

    async fn can_export_pdf(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        self.check_permission(id, user_id, "export_pdf").await
    }

    async fn enable_sharing(&self, id: Uuid) -> Result<Setlist, ApiError> {
        let locked: Option<Option<chrono::NaiveDateTime>> = sqlx::query_scalar(
            "SELECT share_locked_at FROM setlists WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;

        match locked {
            None => return Err(ApiError::NotFound),
            Some(Some(_)) => {
                return Err(ApiError::rule(
                    StatusCode::FORBIDDEN,
                    codes::SHARE_LOCKED,
                    "Public sharing for this setlist was disabled by a moderator.",
                ));
            }
            Some(None) => {}
        }

        let token = crate::utils::share_token::generate_share_token();

        sqlx::query(
            "UPDATE setlists SET share_token = $1 WHERE id = $2 AND share_locked_at IS NULL",
        )
        .bind(&token)
        .bind(id)
        .execute(&self.db)
        .await?;

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
        let setlist = sqlx::query_as::<_, Setlist>(concat!(
            "SELECT ",
            setlist_columns!(),
            ", false AS is_favorite
            FROM setlists s
            WHERE s.share_token = $1 AND s.share_locked_at IS NULL AND s.deleted_at IS NULL"
        ))
        .bind(token)
        .fetch_optional(&self.db)
        .await?;

        Ok(setlist)
    }

    async fn add_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        // Serializes concurrent appends to the same setlist, so two songs
        // added at once can't land on the same position.
        let row: Option<(Option<Uuid>, bool)> = sqlx::query_as(
            "SELECT band_id, is_repertoire FROM setlists WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(setlist_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((band_id, is_repertoire)) = row else {
            return Err(ApiError::NotFound);
        };

        append_song(&mut tx, setlist_id, song_id).await?;

        // Every song a band plays belongs to its repertoire.
        if let (Some(band_id), false) = (band_id, is_repertoire) {
            let repertoire: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM setlists WHERE band_id = $1 AND is_repertoire FOR UPDATE",
            )
            .bind(band_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(repertoire) = repertoire {
                append_song(&mut tx, repertoire, song_id).await?;
            }
        }

        tx.commit().await?;
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
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM setlist_songs ss
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!()
        ))
        .bind(setlist_id)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, SongWithArtist>(concat!(
            "SELECT ",
            song_columns!(),
            ", a.name AS artist_name
             FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            " ORDER BY ss.position ASC
             LIMIT $2 OFFSET $3;"
        ))
        .bind(setlist_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn get_positioned_songs(
        &self,
        setlist_id: Uuid,
    ) -> Result<Vec<(i32, SongWithArtist)>, ApiError> {
        Ok(self
            .song_rows(setlist_id)
            .await?
            .into_iter()
            .map(|row| (row.position, row.song))
            .collect())
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
        limits: Option<QuotaLimits>,
    ) -> Result<(Setlist, i64), ApiError> {
        let original = self
            .find_by_id(id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        let base_title: String = title_override
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("{} (copy)", original.title))
            .chars()
            .take(240)
            .collect();

        // Plain reads, before the write transaction.
        let source_songs = self.song_rows(id).await?;
        let source_markers = self.get_markers(id).await?;

        if let Some(limits) = limits
            && (source_songs.len() + source_markers.len()) as i64 > limits.setlist_items
        {
            return Err(ApiError::quota_exceeded(
                "setlist_items",
                limits.setlist_items,
            ));
        }

        let mut tx = self.db.begin().await?;

        // Personal copy: try the requested title, then fall back to
        // "<title> (2)", "<title> (3)", ... rather than failing outright
        // on the very common case of duplicating the same setlist twice.
        let mut candidate = base_title.clone();
        let mut suffix = 2;
        let final_title = loop {
            let exists = sqlx::query(
                "SELECT id FROM setlists
                 WHERE title = $1 AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL",
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

        let mut new_setlist =
            Setlist::new(&final_title, original.description.clone(), user_id, None);
        new_setlist.links = original.links.clone();

        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NULL, $5, $6, $7)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(sqlx::types::Json(new_setlist.links.to_stored()))
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&mut *tx)
        .await?;

        let mut forker = PersonalForker::new(&mut tx, user_id, limits).await?;
        let mut song_ids: Vec<Uuid> = Vec::with_capacity(source_songs.len());
        let mut positions: Vec<i32> = Vec::with_capacity(source_songs.len());
        let mut skipped = 0i64;
        for row in &source_songs {
            let resolved = if row.song.band_id.is_none() {
                Some(row.song.id)
            } else {
                forker.personal_copy(&mut tx, &row.song).await?
            };
            match resolved {
                Some(song_id) if !song_ids.contains(&song_id) => {
                    song_ids.push(song_id);
                    positions.push(row.position);
                }
                Some(_) => {}
                None => skipped += 1,
            }
        }

        if !song_ids.is_empty() {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position)
                 SELECT $1, t.song_id, t.position FROM UNNEST($2::uuid[], $3::int[]) AS t(song_id, position)
                 ON CONFLICT (setlist_id, song_id) DO NOTHING",
            )
            .bind(new_setlist.id)
            .bind(&song_ids)
            .bind(&positions)
            .execute(&mut *tx)
            .await?;
        }

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
        // every other read path.
        let setlist = self
            .find_by_id(new_setlist.id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        Ok((setlist, skipped))
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
        let (song_rows, markers) =
            tokio::try_join!(self.song_rows(setlist_id), self.get_markers(setlist_id))?;

        let mut items: Vec<SetlistItem> = song_rows
            .into_iter()
            .map(|r| SetlistItem::Song {
                position: r.position,
                song: Box::new(r.song),
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
            .filter(|i| i.item_type == SetlistItemType::Song)
            .map(|i| i.id)
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

    async fn find_repertoire_id(&self, band_id: Uuid) -> Result<Option<Uuid>, ApiError> {
        let id = sqlx::query_scalar("SELECT id FROM setlists WHERE band_id = $1 AND is_repertoire")
            .bind(band_id)
            .fetch_optional(&self.db)
            .await?;
        Ok(id)
    }

    async fn get_repertoire_songs(
        &self,
        band_id: Uuid,
        search: Option<&str>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SongWithArtist>, i64), ApiError> {
        let offset = (page - 1) * size;
        let search = search
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(like_pattern);

        let count = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM setlist_songs ss
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE st.band_id = $1 AND st.is_repertoire AND ",
            scoped_songs!(),
            " AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)"
        ))
        .bind(band_id)
        .bind(&search)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, SongWithArtist>(concat!(
            "SELECT ",
            song_columns!(),
            ", a.name AS artist_name
             FROM setlist_songs ss
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN songs s ON s.id = ss.song_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE st.band_id = $1 AND st.is_repertoire AND ",
            scoped_songs!(),
            " AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)
             ORDER BY LOWER(s.title) ASC, s.id ASC
             LIMIT $3 OFFSET $4"
        ))
        .bind(band_id)
        .bind(&search)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }
}

#[derive(sqlx::FromRow)]
struct SongItemRow {
    position: i32,
    #[sqlx(flatten)]
    song: SongWithArtist,
}

impl SetlistRepositoryImpl {
    /// Live songs of the setlist's own scope, with positions, in order.
    async fn song_rows(&self, setlist_id: Uuid) -> Result<Vec<SongItemRow>, ApiError> {
        let rows = sqlx::query_as::<_, SongItemRow>(concat!(
            "SELECT ss.position, ",
            song_columns!(),
            ", a.name AS artist_name
             FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE ss.setlist_id = $1 AND ",
            scoped_songs!(),
            " ORDER BY ss.position ASC"
        ))
        .bind(setlist_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows)
    }

    /// Owner, or a band member whose role satisfies `permission` (admin
    /// and owner always do). Trashed setlists are not found.
    async fn check_permission(
        &self,
        id: Uuid,
        user_id: Uuid,
        permission: &'static str,
    ) -> Result<(), ApiError> {
        let row = sqlx::query_as::<_, SetlistAccessRow>(
            r#"
            SELECT
                s.user_id AS owner_id,
                s.band_id,
                bm.role AS band_role,
                brp.allowed AS role_permission_allowed
            FROM setlists s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            LEFT JOIN band_role_permissions brp
                ON brp.band_id = s.band_id
                AND brp.role = bm.role
                AND brp.permission = $3::band_permission
            WHERE s.id = $1 AND s.deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(permission)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        // Admin/owner are always allowed; member/moderator follow the
        // band's configurable permission (defaulting to denied if no row
        // exists at all — e.g. a race with band creation).
        let allowed = match row.band_id {
            None => row.owner_id == user_id,
            Some(_) => match row.band_role {
                Some(role) if role.satisfies(BandRole::Admin) => true,
                Some(_) => row.role_permission_allowed.unwrap_or(false),
                None => false,
            },
        };

        match (allowed, row.band_id, row.band_role) {
            (true, _, _) => Ok(()),
            // Someone else's personal setlist, or a band the caller isn't
            // in: don't reveal that it exists.
            (false, None, _) | (false, Some(_), None) => Err(ApiError::NotFound),
            (false, Some(_), Some(_)) => {
                error!(%id, %user_id, permission, "Band member lacks the permission for this setlist.");
                Err(ApiError::Forbidden)
            }
        }
    }

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

/// Copies band songs into one user's personal library while duplicating a
/// setlist, within that transaction, honoring their song/artist quotas.
struct PersonalForker {
    user_id: Uuid,
    limits: Option<QuotaLimits>,
    songs_used: i64,
    artists_used: i64,
}

impl PersonalForker {
    async fn new(
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        limits: Option<QuotaLimits>,
    ) -> Result<Self, ApiError> {
        let (songs_used, artists_used): (i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM songs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                (SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL)",
        )
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
        Ok(Self {
            user_id,
            limits,
            songs_used,
            artists_used,
        })
    }

    /// The caller's personal song matching `source` (same title and
    /// artist name), created when missing. `None` when creating it would
    /// exceed a quota.
    async fn personal_copy(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        source: &SongWithArtist,
    ) -> Result<Option<Uuid>, ApiError> {
        let artist: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND LOWER(name) = LOWER($2)
             ORDER BY created_at LIMIT 1",
        )
        .bind(self.user_id)
        .bind(source.artist_name.trim())
        .fetch_optional(&mut **tx)
        .await?;

        if let Some(artist_id) = artist {
            let existing: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM songs
                 WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND artist_id = $2
                   AND LOWER(TRIM(title)) = LOWER(TRIM($3))
                 LIMIT 1",
            )
            .bind(self.user_id)
            .bind(artist_id)
            .bind(&source.title)
            .fetch_optional(&mut **tx)
            .await?;
            if existing.is_some() {
                return Ok(existing);
            }
        }

        let (song_limit, artist_limit) = match self.limits {
            Some(limits) => (limits.songs, limits.artists),
            None => (i64::MAX, i64::MAX),
        };
        let needs_artist = artist.is_none();
        if self.songs_used + 1 > song_limit
            || (needs_artist && self.artists_used + 1 > artist_limit)
        {
            return Ok(None);
        }

        let artist_id = match artist {
            Some(id) => id,
            None => {
                let new =
                    crate::models::artist::Artist::new(source.artist_name.trim(), self.user_id);
                sqlx::query(
                    "INSERT INTO artists (id, name, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4)",
                )
                .bind(new.id)
                .bind(&new.name)
                .bind(new.user_id)
                .bind(new.created_at)
                .execute(&mut **tx)
                .await?;
                self.artists_used += 1;
                new.id
            }
        };

        let song = Song::fork_for_user(source, artist_id, self.user_id);
        sqlx::query(
            "INSERT INTO songs (id, title, artist_id, user_id, band_id, forked_from, tempo, lyrics, tonality, genre, duration,
                                energy, time_signature, capo, tuning, performance_notes, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16)",
        )
        .bind(song.id)
        .bind(&song.title)
        .bind(song.artist_id)
        .bind(song.user_id)
        .bind(song.tempo)
        .bind(&song.lyrics)
        .bind(song.tonality)
        .bind(song.genre)
        .bind(song.duration)
        .bind(song.energy)
        .bind(&song.time_signature)
        .bind(song.capo)
        .bind(&song.tuning)
        .bind(&song.performance_notes)
        .bind(sqlx::types::Json(song.links.to_stored()))
        .bind(song.created_at)
        .execute(&mut **tx)
        .await?;
        self.songs_used += 1;
        Ok(Some(song.id))
    }
}
