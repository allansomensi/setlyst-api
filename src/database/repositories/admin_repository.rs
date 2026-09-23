//! Cross-tenant queries for the staff console. Nothing here applies
//! ownership or membership filters — callers must have checked the
//! caller's platform role first.

use crate::{
    errors::api_error::ApiError,
    models::admin::{
        AdminBandSummary, AdminListQuery, AdminSetlistSummary, AdminSongSummary, AdminUserBand,
        SharedLink,
    },
};
use sqlx::PgPool;
use uuid::Uuid;

macro_rules! admin_band_columns {
    () => {
        "b.id, b.name, b.slug, b.description, b.logo_url, b.members_can_manage_setlists, b.created_by,
         (SELECT bm.user_id FROM band_members bm WHERE bm.band_id = b.id AND bm.role = 'owner' LIMIT 1) AS owner_id,
         (SELECT u.username FROM band_members bm INNER JOIN users u ON u.id = bm.user_id
            WHERE bm.band_id = b.id AND bm.role = 'owner' LIMIT 1) AS owner_username,
         (SELECT COUNT(*) FROM band_members bm WHERE bm.band_id = b.id) AS member_count,
         (SELECT COUNT(*) FROM setlists s WHERE s.band_id = b.id) AS setlist_count,
         (SELECT COUNT(*) FROM songs s WHERE s.band_id = b.id) AS song_count,
         (SELECT COUNT(*) FROM gigs g WHERE g.band_id = b.id) AS gig_count,
         (SELECT u.username FROM users u WHERE u.id = b.updated_by) AS updated_by_username,
         b.created_at, b.updated_at"
    };
}

macro_rules! admin_song_columns {
    () => {
        "s.id, s.title, s.artist_id, a.name AS artist_name, s.user_id,
         (SELECT u.username FROM users u WHERE u.id = s.user_id) AS owner_username,
         s.band_id, (SELECT b.name FROM bands b WHERE b.id = s.band_id) AS band_name,
         s.tonality, s.tempo, s.genre, s.duration,
         (s.lyrics IS NOT NULL AND LENGTH(TRIM(s.lyrics)) > 0) AS has_lyrics,
         COALESCE((SELECT array_agg(st.tag ORDER BY st.tag) FROM song_tags st WHERE st.song_id = s.id), '{}') AS tags,
         (SELECT COUNT(*) FROM setlist_songs ss WHERE ss.song_id = s.id) AS setlist_count,
         (SELECT u.username FROM users u WHERE u.id = s.updated_by) AS updated_by_username,
         s.created_at, s.updated_at"
    };
}

macro_rules! admin_setlist_columns {
    () => {
        "s.id, s.title, s.description, s.user_id,
         (SELECT u.username FROM users u WHERE u.id = s.user_id) AS owner_username,
         s.band_id, (SELECT b.name FROM bands b WHERE b.id = s.band_id) AS band_name,
         (SELECT COUNT(*) FROM setlist_songs ss WHERE ss.setlist_id = s.id) AS song_count,
         setlist_total_duration(s.id) AS total_duration,
         s.share_token, s.share_locked_at, s.share_lock_reason,
         (SELECT u.username FROM users u WHERE u.id = s.updated_by) AS updated_by_username,
         s.created_at, s.updated_at"
    };
}

#[async_trait::async_trait]
pub trait AdminRepository: Send + Sync {
    async fn list_bands(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminBandSummary>, i64), ApiError>;
    async fn find_band(&self, id: Uuid) -> Result<Option<AdminBandSummary>, ApiError>;
    async fn list_songs(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminSongSummary>, i64), ApiError>;
    async fn find_song(&self, id: Uuid) -> Result<Option<AdminSongSummary>, ApiError>;
    async fn list_setlists(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminSetlistSummary>, i64), ApiError>;
    async fn find_setlist(&self, id: Uuid) -> Result<Option<AdminSetlistSummary>, ApiError>;
    /// Every setlist and gig with an active public link or a staff lock.
    async fn list_shared_links(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<SharedLink>, i64), ApiError>;
    async fn user_bands(&self, user_id: Uuid) -> Result<Vec<AdminUserBand>, ApiError>;
}

pub struct AdminRepositoryImpl {
    pub db: PgPool,
}

impl AdminRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl AdminRepository for AdminRepositoryImpl {
    async fn list_bands(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminBandSummary>, i64), ApiError> {
        let (page, per_page) = query.page();
        let search = query.search_pattern();

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM bands b
             WHERE ($1::text IS NULL OR b.name ILIKE $1 OR b.slug ILIKE $1)
               AND ($2::uuid IS NULL OR EXISTS (
                    SELECT 1 FROM band_members bm WHERE bm.band_id = b.id AND bm.user_id = $2))",
        )
        .bind(&search)
        .bind(query.user_id)
        .fetch_one(&self.db);

        let bands = sqlx::query_as::<_, AdminBandSummary>(concat!(
            "SELECT ",
            admin_band_columns!(),
            " FROM bands b
             WHERE ($1::text IS NULL OR b.name ILIKE $1 OR b.slug ILIKE $1)
               AND ($2::uuid IS NULL OR EXISTS (
                    SELECT 1 FROM band_members bm WHERE bm.band_id = b.id AND bm.user_id = $2))
             ORDER BY LOWER(b.name) ASC
             LIMIT $3 OFFSET $4"
        ))
        .bind(&search)
        .bind(query.user_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);

        let (count, bands) = tokio::try_join!(count, bands)?;
        Ok((bands, count))
    }

    async fn find_band(&self, id: Uuid) -> Result<Option<AdminBandSummary>, ApiError> {
        let band = sqlx::query_as::<_, AdminBandSummary>(concat!(
            "SELECT ",
            admin_band_columns!(),
            " FROM bands b WHERE b.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(band)
    }

    async fn list_songs(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminSongSummary>, i64), ApiError> {
        let (page, per_page) = query.page();
        let search = query.search_pattern();

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM songs s
             INNER JOIN artists a ON a.id = s.artist_id
             INNER JOIN users u ON u.id = s.user_id
             WHERE ($1::text IS NULL OR s.title ILIKE $1 OR a.name ILIKE $1 OR u.username ILIKE $1)
               AND ($2::uuid IS NULL OR s.user_id = $2)
               AND ($3::uuid IS NULL OR s.band_id = $3)",
        )
        .bind(&search)
        .bind(query.user_id)
        .bind(query.band_id)
        .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, AdminSongSummary>(concat!(
            "SELECT ",
            admin_song_columns!(),
            " FROM songs s
             INNER JOIN artists a ON a.id = s.artist_id
             INNER JOIN users u ON u.id = s.user_id
             WHERE ($1::text IS NULL OR s.title ILIKE $1 OR a.name ILIKE $1 OR u.username ILIKE $1)
               AND ($2::uuid IS NULL OR s.user_id = $2)
               AND ($3::uuid IS NULL OR s.band_id = $3)
             ORDER BY s.updated_at DESC, s.id
             LIMIT $4 OFFSET $5"
        ))
        .bind(&search)
        .bind(query.user_id)
        .bind(query.band_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn list_setlists(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<AdminSetlistSummary>, i64), ApiError> {
        let (page, per_page) = query.page();
        let search = query.search_pattern();

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM setlists s
             INNER JOIN users u ON u.id = s.user_id
             WHERE ($1::text IS NULL OR s.title ILIKE $1 OR u.username ILIKE $1)
               AND ($2::uuid IS NULL OR s.user_id = $2)
               AND ($3::uuid IS NULL OR s.band_id = $3)
               AND ($4::boolean IS NULL OR (s.share_token IS NOT NULL) = $4)",
        )
        .bind(&search)
        .bind(query.user_id)
        .bind(query.band_id)
        .bind(query.shared)
        .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, AdminSetlistSummary>(concat!(
            "SELECT ",
            admin_setlist_columns!(),
            " FROM setlists s
             INNER JOIN users u ON u.id = s.user_id
             WHERE ($1::text IS NULL OR s.title ILIKE $1 OR u.username ILIKE $1)
               AND ($2::uuid IS NULL OR s.user_id = $2)
               AND ($3::uuid IS NULL OR s.band_id = $3)
               AND ($4::boolean IS NULL OR (s.share_token IS NOT NULL) = $4)
             ORDER BY s.updated_at DESC, s.id
             LIMIT $5 OFFSET $6"
        ))
        .bind(&search)
        .bind(query.user_id)
        .bind(query.band_id)
        .bind(query.shared)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_song(&self, id: Uuid) -> Result<Option<AdminSongSummary>, ApiError> {
        let song = sqlx::query_as::<_, AdminSongSummary>(concat!(
            "SELECT ",
            admin_song_columns!(),
            " FROM songs s INNER JOIN artists a ON a.id = s.artist_id WHERE s.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(song)
    }

    async fn find_setlist(&self, id: Uuid) -> Result<Option<AdminSetlistSummary>, ApiError> {
        let setlist = sqlx::query_as::<_, AdminSetlistSummary>(concat!(
            "SELECT ",
            admin_setlist_columns!(),
            " FROM setlists s WHERE s.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn list_shared_links(
        &self,
        query: &AdminListQuery,
    ) -> Result<(Vec<SharedLink>, i64), ApiError> {
        let (page, per_page) = query.page();
        let search = query.search_pattern();

        // One UNION over both shareable kinds, then filtered/paged as a
        // whole so the listing reads as a single timeline.
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM (
                SELECT s.title AS title, s.user_id AS owner_id, s.share_locked_at FROM setlists s
                WHERE s.share_token IS NOT NULL OR s.share_locked_at IS NOT NULL
                UNION ALL
                SELECT g.venue AS title, g.user_id AS owner_id, g.share_locked_at FROM gigs g
                WHERE g.share_token IS NOT NULL OR g.share_locked_at IS NOT NULL
             ) l
             INNER JOIN users u ON u.id = l.owner_id
             WHERE ($1::text IS NULL OR l.title ILIKE $1 OR u.username ILIKE $1)
               AND ($2::bool IS NULL OR (l.share_locked_at IS NULL) = $2)",
        )
        .bind(&search)
        .bind(query.shared)
        .fetch_one(&self.db);

        let links = sqlx::query_as::<_, SharedLink>(
            "SELECT l.kind, l.id, l.title, l.owner_id, u.username AS owner_username,
                    l.band_id, (SELECT b.name FROM bands b WHERE b.id = l.band_id) AS band_name,
                    l.share_token, l.share_locked_at, l.share_lock_reason,
                    (SELECT x.username FROM users x WHERE x.id = l.share_locked_by) AS share_locked_by_username,
                    l.updated_at
             FROM (
                SELECT 'setlist'::text AS kind, s.id, s.title, s.user_id AS owner_id, s.band_id,
                       s.share_token, s.share_locked_at, s.share_lock_reason, s.share_locked_by, s.updated_at
                FROM setlists s
                WHERE s.share_token IS NOT NULL OR s.share_locked_at IS NOT NULL
                UNION ALL
                SELECT 'gig'::text AS kind, g.id, g.venue AS title, g.user_id AS owner_id, g.band_id,
                       g.share_token, g.share_locked_at, g.share_lock_reason, g.share_locked_by, g.updated_at
                FROM gigs g
                WHERE g.share_token IS NOT NULL OR g.share_locked_at IS NOT NULL
             ) l
             INNER JOIN users u ON u.id = l.owner_id
             WHERE ($1::text IS NULL OR l.title ILIKE $1 OR u.username ILIKE $1)
               AND ($4::bool IS NULL OR (l.share_locked_at IS NULL) = $4)
             ORDER BY (l.share_locked_at IS NULL) DESC, l.updated_at DESC
             LIMIT $2 OFFSET $3",
        )
        .bind(&search)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .bind(query.shared)
        .fetch_all(&self.db);

        let (count, links) = tokio::try_join!(count, links)?;
        Ok((links, count))
    }

    async fn user_bands(&self, user_id: Uuid) -> Result<Vec<AdminUserBand>, ApiError> {
        let bands = sqlx::query_as::<_, AdminUserBand>(
            "SELECT b.id AS band_id, b.name AS band_name, bm.role, bm.joined_at
             FROM band_members bm INNER JOIN bands b ON b.id = bm.band_id
             WHERE bm.user_id = $1
             ORDER BY bm.role DESC, LOWER(b.name) ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;
        Ok(bands)
    }
}
