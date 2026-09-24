use crate::database::repositories::quota_repository::QuotaGuard;
use crate::{
    errors::api_error::ApiError,
    models::{
        band::BandRole,
        link::Links,
        song::{
            CreateSongPayload, Song, SongExport, SongSetlistRef, SongWithArtist, TagCount,
            UpdateSongPayload, clean_text,
        },
    },
    validations::link::normalize_links,
};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`Song`] from `songs s`, including its tags and
/// the last editor's username. A macro so it can be spliced into
/// `concat!` (sqlx only accepts literal query strings).
macro_rules! song_columns {
    () => {
        "s.id, s.title, s.artist_id, s.user_id, s.band_id, s.forked_from, s.tempo, s.lyrics,
         s.tonality, s.genre, s.duration, s.energy, s.time_signature, s.capo, s.tuning,
         s.performance_notes, s.links,
         COALESCE((SELECT array_agg(st.tag ORDER BY st.tag) FROM song_tags st WHERE st.song_id = s.id), '{}') AS tags,
         s.updated_by,
         (SELECT u.username FROM users u WHERE u.id = s.updated_by) AS updated_by_username,
         s.created_at, s.updated_at"
    };
}
pub(crate) use song_columns;

/// Filters for listing a user's personal songs.
#[derive(Debug, Default, Clone)]
pub struct SongFilter {
    /// Case-insensitive substring over title and artist name.
    pub search: Option<String>,
    /// Songs must carry every one of these (already normalized) tags.
    pub tags: Vec<String>,
}

#[async_trait::async_trait]
pub trait SongRepository: Send + Sync {
    /// Lists the caller's personal songs (i.e. `band_id IS NULL`).
    async fn find_all(
        &self,
        user_id: Uuid,
        filter: &SongFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Song>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Song>, ApiError>;
    /// Any song by ID, without an ownership filter (staff tooling).
    async fn find_any(&self, id: Uuid) -> Result<Option<Song>, ApiError>;
    /// Live setlists `user_id` can see (personal ones, and those of
    /// their bands) in which the song counts: personal setlists first,
    /// then by band with the repertoire first, then by title.
    async fn setlists_of(&self, id: Uuid, user_id: Uuid) -> Result<Vec<SongSetlistRef>, ApiError>;
    /// Creates a personal song. `quota` is enforced inside the insert's
    /// transaction (see [`QuotaGuard`]).
    async fn create(
        &self,
        payload: &CreateSongPayload,
        tags: &[String],
        user_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Song, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSongPayload,
        tags: Option<&[String]>,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes a song (staff tooling and the trash).
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Moves a live song to the trash.
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Checks title uniqueness among the caller's *personal* songs for that artist.
    async fn is_unique(
        &self,
        title: &str,
        artist_id: Uuid,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    /// The song exists and is one of `user_id`'s *personal* songs.
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may edit/delete this song: its personal owner, or
    /// — for a band-owned copy — a band member whose role satisfies the
    /// band's `manage_songs` permission.
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    async fn export_all_chordpro(&self, user_id: Uuid) -> Result<Vec<SongExport>, ApiError>;
    /// Fetches any song by ID (no ownership filter) with its artist name
    /// resolved. The caller must already have authorized access to `id`.
    async fn find_with_artist_name(&self, id: Uuid) -> Result<Option<SongWithArtist>, ApiError>;
    /// Creates (or reuses) the band-owned copy of `source` under `band_id`.
    /// `quota` (the band's song limit) is enforced in the insert's
    /// transaction, so concurrent forks can't take the band over it.
    async fn create_band_copy(
        &self,
        source: &SongWithArtist,
        band_id: Uuid,
        artist_id: Uuid,
        creator_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Song, ApiError>;
    /// Whether `band_id` already holds a copy forked from `source_id`.
    async fn has_band_fork(&self, band_id: Uuid, source_id: Uuid) -> Result<bool, ApiError>;
    /// The live copy of `source_id` held by `band_id`, if any.
    async fn find_band_fork(
        &self,
        band_id: Uuid,
        source_id: Uuid,
    ) -> Result<Option<Uuid>, ApiError>;
    /// The caller's personal tag vocabulary with usage counts.
    async fn list_tags(&self, user_id: Uuid) -> Result<Vec<TagCount>, ApiError>;
    /// Renames (or merges into an existing) tag across the caller's
    /// personal songs. Returns how many songs changed.
    async fn rename_tag(&self, user_id: Uuid, from: &str, to: &str) -> Result<u64, ApiError>;
    /// Removes a tag from every personal song of the caller.
    async fn delete_tag(&self, user_id: Uuid, tag: &str) -> Result<u64, ApiError>;
}

pub struct SongRepositoryImpl {
    pub db: PgPool,
}

/// Row shape used to decide edit permission on a song without fetching its
/// full column set.
#[derive(sqlx::FromRow)]
struct SongAccessRow {
    owner_id: Uuid,
    band_id: Option<Uuid>,
    band_role: Option<BandRole>,
    role_permission_allowed: Option<bool>,
}

impl SongRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

async fn replace_tags(
    tx: &mut Transaction<'_, Postgres>,
    song_id: Uuid,
    tags: &[String],
) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM song_tags WHERE song_id = $1")
        .bind(song_id)
        .execute(&mut **tx)
        .await?;

    if !tags.is_empty() {
        sqlx::query(
            "INSERT INTO song_tags (song_id, tag, created_at)
             SELECT $1, t, $3 FROM UNNEST($2::text[]) AS t
             ON CONFLICT DO NOTHING",
        )
        .bind(song_id)
        .bind(tags)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// `%search%` for `ILIKE`, with the pattern's own wildcards escaped.
pub(crate) fn like_pattern(search: &str) -> String {
    format!(
        "%{}%",
        search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

#[async_trait::async_trait]
impl SongRepository for SongRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        filter: &SongFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Song>, i64), ApiError> {
        let offset = (page - 1) * size;
        let search = filter
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(like_pattern);
        let tags: &[String] = &filter.tags;

        // `cardinality($3) = 0` short-circuits the tag filter when no tags
        // were requested; otherwise every requested tag must be present.
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM songs s
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
               AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)
               AND (cardinality($3::text[]) = 0 OR (
                    SELECT COUNT(DISTINCT st.tag) FROM song_tags st
                    WHERE st.song_id = s.id AND st.tag = ANY($3)
               ) = cardinality($3::text[]))",
        )
        .bind(user_id)
        .bind(&search)
        .bind(tags)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, Song>(concat!(
            "SELECT ",
            song_columns!(),
            ", a.name AS artist_name
             FROM songs s
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
               AND ($2::text IS NULL OR s.title ILIKE $2 OR a.name ILIKE $2)
               AND (cardinality($3::text[]) = 0 OR (
                    SELECT COUNT(DISTINCT st.tag) FROM song_tags st
                    WHERE st.song_id = s.id AND st.tag = ANY($3)
               ) = cardinality($3::text[]))
             ORDER BY LOWER(s.title) ASC, s.id ASC
             LIMIT $4 OFFSET $5"
        ))
        .bind(user_id)
        .bind(&search)
        .bind(tags)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Song>, ApiError> {
        // Personal songs are only visible to their owner. Band-owned songs
        // are visible to any member of that band, regardless of role —
        // editing them is gated separately by `can_manage`.
        let song = sqlx::query_as::<_, Song>(concat!(
            "SELECT ",
            song_columns!(),
            ", (SELECT a.name FROM artists a WHERE a.id = s.artist_id) AS artist_name
             FROM songs s
             LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
             WHERE s.id = $1 AND s.deleted_at IS NULL AND (
                (s.band_id IS NULL AND s.user_id = $2)
                OR (s.band_id IS NOT NULL AND bm.user_id IS NOT NULL)
             )"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(song)
    }

    async fn setlists_of(&self, id: Uuid, user_id: Uuid) -> Result<Vec<SongSetlistRef>, ApiError> {
        // Same scope rule as a setlist's own song list: a personal setlist
        // only counts its owner's personal songs, a band setlist only the
        // band's songs.
        let setlists = sqlx::query_as::<_, SongSetlistRef>(
            "SELECT st.id, st.title, st.is_repertoire, st.band_id, b.name AS band_name, ss.position
             FROM setlist_songs ss
             INNER JOIN setlists st ON st.id = ss.setlist_id
             INNER JOIN songs s ON s.id = ss.song_id
             LEFT JOIN bands b ON b.id = st.band_id
             WHERE ss.song_id = $1 AND st.deleted_at IS NULL AND s.deleted_at IS NULL
               AND ((st.band_id IS NULL AND s.band_id IS NULL AND s.user_id = st.user_id)
                    OR s.band_id = st.band_id)
               AND ((st.band_id IS NULL AND st.user_id = $2)
                    OR EXISTS (SELECT 1 FROM band_members bm
                               WHERE bm.band_id = st.band_id AND bm.user_id = $2))
             ORDER BY st.band_id IS NOT NULL, LOWER(b.name), st.band_id,
                      st.is_repertoire DESC, LOWER(st.title), st.id",
        )
        .bind(id)
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;
        Ok(setlists)
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Song>, ApiError> {
        let song = sqlx::query_as::<_, Song>(concat!(
            "SELECT ",
            song_columns!(),
            ", (SELECT a.name FROM artists a WHERE a.id = s.artist_id) AS artist_name
             FROM songs s WHERE s.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(song)
    }

    async fn create(
        &self,
        payload: &CreateSongPayload,
        tags: &[String],
        user_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Song, ApiError> {
        let links = normalize_links(payload.links.as_deref().unwrap_or_default())?;
        let mut new_song = Song::new(payload, user_id);
        new_song.title = new_song.title.trim().to_string();
        new_song.lyrics = new_song.lyrics.filter(|l| !l.trim().is_empty());
        new_song.tags = tags.to_vec();
        new_song.links = Links::from_stored(links.clone());

        let mut tx = self.db.begin().await?;
        QuotaGuard::enforce_all(quota, &mut tx).await?;

        sqlx::query(
            "INSERT INTO songs (id, title, artist_id, user_id, band_id, forked_from, tempo, lyrics, tonality, genre, duration,
                                energy, time_signature, capo, tuning, performance_notes, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
        )
        .bind(new_song.id)
        .bind(&new_song.title)
        .bind(new_song.artist_id)
        .bind(new_song.user_id)
        .bind(new_song.band_id)
        .bind(new_song.forked_from)
        .bind(new_song.tempo)
        .bind(&new_song.lyrics)
        .bind(new_song.tonality)
        .bind(new_song.genre)
        .bind(new_song.duration)
        .bind(new_song.energy)
        .bind(&new_song.time_signature)
        .bind(new_song.capo)
        .bind(&new_song.tuning)
        .bind(&new_song.performance_notes)
        .bind(sqlx::types::Json(&links))
        .bind(new_song.created_at)
        .bind(new_song.updated_at)
        .execute(&mut *tx)
        .await?;

        replace_tags(&mut tx, new_song.id, tags).await?;
        new_song.artist_name = sqlx::query_scalar("SELECT name FROM artists WHERE id = $1")
            .bind(new_song.artist_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;

        Ok(new_song)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateSongPayload,
        tags: Option<&[String]>,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE songs SET title = $1 WHERE id = $2")
                .bind(title.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(artist_id) = payload.artist_id {
            sqlx::query("UPDATE songs SET artist_id = $1 WHERE id = $2")
                .bind(artist_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(tempo) = payload.tempo {
            sqlx::query("UPDATE songs SET tempo = $1 WHERE id = $2")
                .bind(tempo)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(lyrics) = &payload.lyrics {
            // An all-whitespace chart is the same as no chart.
            let lyrics = lyrics.as_ref().filter(|l| !l.trim().is_empty());
            sqlx::query("UPDATE songs SET lyrics = $1 WHERE id = $2")
                .bind(lyrics)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(tonality) = payload.tonality {
            sqlx::query("UPDATE songs SET tonality = $1 WHERE id = $2")
                .bind(tonality)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(genre) = payload.genre {
            sqlx::query("UPDATE songs SET genre = $1 WHERE id = $2")
                .bind(genre)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(duration) = payload.duration {
            sqlx::query("UPDATE songs SET duration = $1 WHERE id = $2")
                .bind(duration)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(energy) = payload.energy {
            sqlx::query("UPDATE songs SET energy = $1 WHERE id = $2")
                .bind(energy)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(time_signature) = &payload.time_signature {
            sqlx::query("UPDATE songs SET time_signature = $1 WHERE id = $2")
                .bind(clean_text(time_signature.as_deref()))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(capo) = payload.capo {
            sqlx::query("UPDATE songs SET capo = $1 WHERE id = $2")
                .bind(capo)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(tuning) = &payload.tuning {
            sqlx::query("UPDATE songs SET tuning = $1 WHERE id = $2")
                .bind(clean_text(tuning.as_deref()))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(notes) = &payload.performance_notes {
            sqlx::query("UPDATE songs SET performance_notes = $1 WHERE id = $2")
                .bind(clean_text(notes.as_deref()))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(links) = &payload.links {
            let links = normalize_links(links)?;
            sqlx::query("UPDATE songs SET links = $1 WHERE id = $2")
                .bind(sqlx::types::Json(&links))
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(tags) = tags {
            replace_tags(&mut tx, id, tags).await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE songs SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM songs WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE songs SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL",
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

    async fn is_unique(
        &self,
        title: &str,
        artist_id: Uuid,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        // Case- and whitespace-insensitive: "Wonderwall" and "wonderwall "
        // by the same artist are the same song.
        let exists = sqlx::query(
            "SELECT id FROM songs
             WHERE LOWER(TRIM(title)) = LOWER(TRIM($1)) AND artist_id = $2 AND user_id = $3
               AND band_id IS NULL AND deleted_at IS NULL AND ($4::uuid IS NULL OR id != $4)",
        )
        .bind(title)
        .bind(artist_id)
        .bind(user_id)
        .bind(exclude_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if exists {
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists =
            sqlx::query("SELECT id FROM songs WHERE id = $1 AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL;")
                .bind(id)
                .bind(user_id)
                .fetch_optional(&self.db)
                .await?
                .is_some();

        if exists {
            Ok(())
        } else {
            Err(ApiError::NotFound)
        }
    }

    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let row = sqlx::query_as::<_, SongAccessRow>(
            r#"
            SELECT
                s.user_id AS owner_id,
                s.band_id,
                bm.role AS band_role,
                brp.allowed AS role_permission_allowed
            FROM songs s
            LEFT JOIN band_members bm ON bm.band_id = s.band_id AND bm.user_id = $2
            LEFT JOIN band_role_permissions brp
                ON brp.band_id = s.band_id
                AND brp.role = bm.role
                AND brp.permission = 'manage_songs'
            WHERE s.id = $1 AND s.deleted_at IS NULL
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
                Some(role) if role.satisfies(BandRole::Admin) => true,
                Some(_) => row.role_permission_allowed.unwrap_or(false),
                None => false,
            },
        };

        match (allowed, row.band_id, row.band_role) {
            (true, _, _) => Ok(()),
            // Not the owner of a personal song, or not a member of the band:
            // don't reveal that the song exists at all.
            (false, None, _) | (false, Some(_), None) => Err(ApiError::NotFound),
            (false, Some(_), Some(_)) => {
                error!(%id, %user_id, "User is not allowed to manage this song.");
                Err(ApiError::Forbidden)
            }
        }
    }

    async fn export_all_chordpro(&self, user_id: Uuid) -> Result<Vec<SongExport>, ApiError> {
        let songs = sqlx::query_as::<_, SongExport>(
            r#"
            SELECT
                s.title,
                a.name AS artist_name,
                s.tonality::text AS tonality,
                s.tempo,
                s.lyrics,
                s.time_signature,
                s.capo,
                s.duration,
                s.energy
            FROM songs s
            LEFT JOIN artists a ON s.artist_id = a.id
            WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
            ORDER BY a.name ASC, s.title ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        Ok(songs)
    }

    async fn find_with_artist_name(&self, id: Uuid) -> Result<Option<SongWithArtist>, ApiError> {
        let song = sqlx::query_as::<_, SongWithArtist>(concat!(
            "SELECT ",
            song_columns!(),
            ", a.name AS artist_name
             FROM songs s
             INNER JOIN artists a ON a.id = s.artist_id
             WHERE s.id = $1 AND s.deleted_at IS NULL"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(song)
    }

    async fn create_band_copy(
        &self,
        source: &SongWithArtist,
        band_id: Uuid,
        artist_id: Uuid,
        creator_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Song, ApiError> {
        let new_song = Song::fork_for_band(source, band_id, artist_id, creator_id);
        let mut tx = self.db.begin().await?;

        // An existing copy (checked again under the lock) costs nothing;
        // a new one counts against the band's song quota.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM songs
             WHERE band_id = $1 AND forked_from = $2 AND deleted_at IS NULL",
        )
        .bind(band_id)
        .bind(source.id)
        .fetch_optional(&mut *tx)
        .await?;
        if existing.is_none() {
            QuotaGuard::enforce_all(quota, &mut tx).await?;
        }

        // If this exact source song was already forked into this band,
        // reuse that copy instead of creating a duplicate — atomically, via
        // the partial unique index on (band_id, forked_from).
        let (song_id, inserted): (Uuid, bool) = sqlx::query_as(
            "INSERT INTO songs (id, title, artist_id, user_id, band_id, forked_from, tempo, lyrics, tonality, genre, duration,
                                energy, time_signature, capo, tuning, performance_notes, links, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
             ON CONFLICT (band_id, forked_from)
                WHERE band_id IS NOT NULL AND forked_from IS NOT NULL AND deleted_at IS NULL
             DO UPDATE SET updated_at = songs.updated_at
             RETURNING id, (xmax = 0) AS inserted",
        )
        .bind(new_song.id)
        .bind(&new_song.title)
        .bind(new_song.artist_id)
        .bind(new_song.user_id)
        .bind(new_song.band_id)
        .bind(new_song.forked_from)
        .bind(new_song.tempo)
        .bind(&new_song.lyrics)
        .bind(new_song.tonality)
        .bind(new_song.genre)
        .bind(new_song.duration)
        .bind(new_song.energy)
        .bind(&new_song.time_signature)
        .bind(new_song.capo)
        .bind(&new_song.tuning)
        .bind(&new_song.performance_notes)
        .bind(sqlx::types::Json(new_song.links.to_stored()))
        .bind(new_song.created_at)
        .bind(new_song.updated_at)
        .fetch_one(&mut *tx)
        .await?;

        // A fresh copy carries the source's tags along.
        if inserted {
            replace_tags(&mut tx, song_id, &source.tags).await?;
        }
        tx.commit().await?;

        self.find_any(song_id).await?.ok_or(ApiError::NotFound)
    }

    async fn has_band_fork(&self, band_id: Uuid, source_id: Uuid) -> Result<bool, ApiError> {
        Ok(self.find_band_fork(band_id, source_id).await?.is_some())
    }

    async fn find_band_fork(
        &self,
        band_id: Uuid,
        source_id: Uuid,
    ) -> Result<Option<Uuid>, ApiError> {
        let id = sqlx::query_scalar(
            "SELECT id FROM songs WHERE band_id = $1 AND forked_from = $2 AND deleted_at IS NULL",
        )
        .bind(band_id)
        .bind(source_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(id)
    }

    async fn list_tags(&self, user_id: Uuid) -> Result<Vec<TagCount>, ApiError> {
        let tags = sqlx::query_as::<_, TagCount>(
            "SELECT st.tag, COUNT(*) AS song_count
             FROM song_tags st
             INNER JOIN songs s ON s.id = st.song_id
             WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
             GROUP BY st.tag
             ORDER BY song_count DESC, st.tag ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;
        Ok(tags)
    }

    async fn rename_tag(&self, user_id: Uuid, from: &str, to: &str) -> Result<u64, ApiError> {
        let mut tx = self.db.begin().await?;

        // Songs that already have `to` just lose `from` (a merge)...
        sqlx::query(
            "DELETE FROM song_tags st USING songs s
             WHERE st.song_id = s.id AND s.user_id = $1 AND s.band_id IS NULL AND st.tag = $2
               AND EXISTS (SELECT 1 FROM song_tags o WHERE o.song_id = st.song_id AND o.tag = $3)",
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;

        // ...the rest are renamed in place.
        let renamed = sqlx::query(
            "UPDATE song_tags st SET tag = $3
             FROM songs s
             WHERE st.song_id = s.id AND s.user_id = $1 AND s.band_id IS NULL AND st.tag = $2",
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(renamed.rows_affected())
    }

    async fn delete_tag(&self, user_id: Uuid, tag: &str) -> Result<u64, ApiError> {
        let result = sqlx::query(
            "DELETE FROM song_tags st USING songs s
             WHERE st.song_id = s.id AND s.user_id = $1 AND s.band_id IS NULL AND st.tag = $2",
        )
        .bind(user_id)
        .bind(tag)
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected())
    }
}
