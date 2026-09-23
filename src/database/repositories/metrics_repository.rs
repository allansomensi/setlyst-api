use crate::{
    errors::api_error::ApiError,
    models::metrics::{
        AdminMetrics, AdminTimeseries, ArtistSongCount, GenreCount, RoleCount, TimeseriesPoint,
        UserMetrics, UserTimeseries,
    },
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct UserCountsRow {
    total_artists: Option<i64>,
    total_songs: Option<i64>,
    total_setlists: Option<i64>,
    total_bands: Option<i64>,
    songs_with_lyrics: Option<i64>,
    songs_without_lyrics: Option<i64>,
    songs_with_tonality: Option<i64>,
    songs_with_tempo: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct AdminCountsRow {
    total_users: Option<i64>,
    total_artists: Option<i64>,
    total_songs: Option<i64>,
    total_setlists: Option<i64>,
    total_bands: Option<i64>,
    songs_with_lyrics: Option<i64>,
    songs_without_lyrics: Option<i64>,
    active_users: Option<i64>,
    inactive_users: Option<i64>,
}

#[async_trait::async_trait]
pub trait MetricsRepository: Send + Sync {
    async fn get_user_metrics(&self, user_id: Uuid) -> Result<UserMetrics, ApiError>;
    async fn get_admin_metrics(&self) -> Result<AdminMetrics, ApiError>;
    /// Daily activity counts for the caller's own content over the
    /// trailing `days` days (gap-filled — every day appears, even at 0).
    async fn get_user_timeseries(
        &self,
        user_id: Uuid,
        days: i64,
    ) -> Result<UserTimeseries, ApiError>;
    /// Daily platform-wide activity counts over the trailing `days` days.
    async fn get_admin_timeseries(&self, days: i64) -> Result<AdminTimeseries, ApiError>;
}

pub struct MetricsRepositoryImpl {
    pub db: PgPool,
}

impl MetricsRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl MetricsRepository for MetricsRepositoryImpl {
    async fn get_user_metrics(&self, user_id: Uuid) -> Result<UserMetrics, ApiError> {
        let counts_fut = sqlx::query_as::<_, UserCountsRow>(
            "SELECT
                (SELECT COUNT(*) FROM artists  WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL) AS total_artists,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL) AS total_songs,
                (SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL) AS total_setlists,
                (SELECT COUNT(*) FROM band_members WHERE user_id = $1) AS total_bands,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND lyrics IS NOT NULL AND lyrics <> '') AS songs_with_lyrics,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND (lyrics IS NULL OR lyrics = ''))     AS songs_without_lyrics,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND tonality IS NOT NULL)                AS songs_with_tonality,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND tempo IS NOT NULL)                   AS songs_with_tempo"
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let genres_fut = sqlx::query_as::<_, GenreCount>(
            "SELECT genre::text AS genre, COUNT(*) AS count
             FROM songs
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL AND genre IS NOT NULL
             GROUP BY genre
             ORDER BY COUNT(*) DESC
             LIMIT 5",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let artists_fut = sqlx::query_as::<_, ArtistSongCount>(
            "SELECT a.name AS artist_name, COUNT(s.id) AS song_count
             FROM artists a
             LEFT JOIN songs s ON s.artist_id = a.id AND s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
             WHERE a.user_id = $1 AND a.band_id IS NULL AND a.deleted_at IS NULL
             GROUP BY a.id, a.name
             ORDER BY COUNT(s.id) DESC
             LIMIT 5",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let (counts, top_genres, top_artists_by_songs) =
            tokio::try_join!(counts_fut, genres_fut, artists_fut)?;

        Ok(UserMetrics {
            total_artists: counts.total_artists.unwrap_or(0),
            total_songs: counts.total_songs.unwrap_or(0),
            total_setlists: counts.total_setlists.unwrap_or(0),
            total_bands: counts.total_bands.unwrap_or(0),
            songs_with_lyrics: counts.songs_with_lyrics.unwrap_or(0),
            songs_without_lyrics: counts.songs_without_lyrics.unwrap_or(0),
            songs_with_tonality: counts.songs_with_tonality.unwrap_or(0),
            songs_with_tempo: counts.songs_with_tempo.unwrap_or(0),
            top_genres,
            top_artists_by_songs,
        })
    }

    async fn get_admin_metrics(&self) -> Result<AdminMetrics, ApiError> {
        // Forked band copies (songs.forked_from / artists.forked_from set)
        // are independent DB rows by design — see migrations 0011 and
        // 0018 — but they represent the same creative content as the row
        // they were forked from, not new content. Excluding them from the
        // platform-wide totals avoids the count doubling every time a
        // member forks their existing songs into a band.
        let counts_fut = sqlx::query_as::<_, AdminCountsRow>(
            "SELECT
                (SELECT COUNT(*) FROM users)    AS total_users,
                (SELECT COUNT(*) FROM artists WHERE forked_from IS NULL AND deleted_at IS NULL) AS total_artists,
                (SELECT COUNT(*) FROM songs   WHERE forked_from IS NULL AND deleted_at IS NULL) AS total_songs,
                (SELECT COUNT(*) FROM setlists WHERE deleted_at IS NULL AND NOT is_repertoire) AS total_setlists,
                (SELECT COUNT(*) FROM bands)    AS total_bands,
                (SELECT COUNT(*) FROM songs WHERE forked_from IS NULL AND deleted_at IS NULL AND lyrics IS NOT NULL AND lyrics <> '') AS songs_with_lyrics,
                (SELECT COUNT(*) FROM songs WHERE forked_from IS NULL AND deleted_at IS NULL AND (lyrics IS NULL OR lyrics = ''))     AS songs_without_lyrics,
                (SELECT COUNT(*) FROM users WHERE status = 'active')                   AS active_users,
                (SELECT COUNT(*) FROM users WHERE status = 'inactive')                 AS inactive_users"
        )
        .fetch_one(&self.db);

        let genres_fut = sqlx::query_as::<_, GenreCount>(
            "SELECT genre::text AS genre, COUNT(*) AS count
             FROM songs
             WHERE genre IS NOT NULL AND forked_from IS NULL AND deleted_at IS NULL
             GROUP BY genre
             ORDER BY COUNT(*) DESC
             LIMIT 5",
        )
        .fetch_all(&self.db);

        let roles_fut = sqlx::query_as::<_, RoleCount>(
            "SELECT role::text AS role, COUNT(*) AS count
             FROM users
             GROUP BY role
             ORDER BY COUNT(*) DESC",
        )
        .fetch_all(&self.db);

        let (counts, top_genres, users_by_role) =
            tokio::try_join!(counts_fut, genres_fut, roles_fut)?;

        Ok(AdminMetrics {
            total_users: counts.total_users.unwrap_or(0),
            total_artists: counts.total_artists.unwrap_or(0),
            total_songs: counts.total_songs.unwrap_or(0),
            total_setlists: counts.total_setlists.unwrap_or(0),
            total_bands: counts.total_bands.unwrap_or(0),
            songs_with_lyrics: counts.songs_with_lyrics.unwrap_or(0),
            songs_without_lyrics: counts.songs_without_lyrics.unwrap_or(0),
            active_users: counts.active_users.unwrap_or(0),
            inactive_users: counts.inactive_users.unwrap_or(0),
            top_genres,
            users_by_role,
        })
    }

    async fn get_user_timeseries(
        &self,
        user_id: Uuid,
        days: i64,
    ) -> Result<UserTimeseries, ApiError> {
        let songs_fut = self.daily_counts_for_user(
            "songs",
            "t.user_id = $2 AND t.band_id IS NULL AND t.deleted_at IS NULL",
            days,
            user_id,
        );
        let setlists_fut = self.daily_counts_for_user(
            "setlists",
            "t.user_id = $2 AND t.band_id IS NULL AND t.deleted_at IS NULL",
            days,
            user_id,
        );
        let gigs_fut = self.daily_counts_for_user(
            "gigs",
            "t.user_id = $2 AND t.band_id IS NULL AND t.deleted_at IS NULL",
            days,
            user_id,
        );

        let (songs_created, setlists_created, gigs_created) =
            tokio::try_join!(songs_fut, setlists_fut, gigs_fut)?;

        Ok(UserTimeseries {
            songs_created,
            setlists_created,
            gigs_created,
        })
    }

    async fn get_admin_timeseries(&self, days: i64) -> Result<AdminTimeseries, ApiError> {
        let users_fut = self.daily_counts_global("users", None, days);
        // Exclude forked band copies (see get_admin_metrics) so this trend
        // reflects genuinely new songs, not a spike every time someone
        // forks their existing songs into a band setlist.
        let songs_fut = self.daily_counts_global(
            "songs",
            Some("t.forked_from IS NULL AND t.deleted_at IS NULL"),
            days,
        );
        let setlists_fut = self.daily_counts_global(
            "setlists",
            Some("t.deleted_at IS NULL AND NOT t.is_repertoire"),
            days,
        );
        let bands_fut = self.daily_counts_global("bands", None, days);

        let (users_registered, songs_created, setlists_created, bands_created) =
            tokio::try_join!(users_fut, songs_fut, setlists_fut, bands_fut)?;

        Ok(AdminTimeseries {
            users_registered,
            songs_created,
            setlists_created,
            bands_created,
        })
    }
}

impl MetricsRepositoryImpl {
    /// Builds and runs a gap-filled daily-count query: one row per day in
    /// the trailing `days`-day window (today inclusive), `count` being how
    /// many `table` rows have `created_at::date` equal to that day. Days
    /// with no rows still appear, with `count: 0` — charts rely on there
    /// being no gaps in the series. `extra_where`, if given, is ANDed into
    /// the join condition (e.g. to exclude forked band copies).
    ///
    /// `table` and `extra_where` are Rust string literals supplied by this
    /// file only (never request input), so interpolating them directly is
    /// safe; `days` is always the query's only bind parameter.
    async fn daily_counts_global(
        &self,
        table: &str,
        extra_where: Option<&str>,
        days: i64,
    ) -> Result<Vec<TimeseriesPoint>, ApiError> {
        let join_condition = match extra_where {
            Some(extra) => format!("t.created_at::date = gs.day::date AND {extra}"),
            None => "t.created_at::date = gs.day::date".to_string(),
        };

        let sql = format!(
            r#"
            SELECT gs.day::date AS date, COUNT(t.id) AS count
            FROM generate_series(
                (CURRENT_DATE - ($1::int - 1))::timestamp,
                CURRENT_DATE::timestamp,
                interval '1 day'
            ) AS gs(day)
            LEFT JOIN {table} t ON {join_condition}
            GROUP BY gs.day
            ORDER BY gs.day
            "#
        );

        let points = sqlx::query_as::<_, TimeseriesPoint>(sqlx::AssertSqlSafe(sql))
            .bind(days)
            .fetch_all(&self.db)
            .await?;

        Ok(points)
    }

    /// Same as [`Self::daily_counts_global`], additionally scoped by
    /// `extra_where` (a `WHERE` clause fragment referencing `t` and the
    /// bound `$2 = user_id`, e.g. `"user_id = $2 AND band_id IS NULL"`).
    async fn daily_counts_for_user(
        &self,
        table: &str,
        extra_where: &str,
        days: i64,
        user_id: Uuid,
    ) -> Result<Vec<TimeseriesPoint>, ApiError> {
        let sql = format!(
            r#"
            SELECT gs.day::date AS date, COUNT(t.id) AS count
            FROM generate_series(
                (CURRENT_DATE - ($1::int - 1))::timestamp,
                CURRENT_DATE::timestamp,
                interval '1 day'
            ) AS gs(day)
            LEFT JOIN {table} t ON t.created_at::date = gs.day::date AND {extra_where}
            GROUP BY gs.day
            ORDER BY gs.day
            "#
        );

        let points = sqlx::query_as::<_, TimeseriesPoint>(sqlx::AssertSqlSafe(sql))
            .bind(days)
            .bind(user_id)
            .fetch_all(&self.db)
            .await?;

        Ok(points)
    }
}
