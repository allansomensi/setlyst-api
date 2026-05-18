use crate::{
    errors::api_error::ApiError,
    models::metrics::{AdminMetrics, ArtistSongCount, GenreCount, RoleCount, UserMetrics},
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct UserCountsRow {
    total_artists: Option<i64>,
    total_songs: Option<i64>,
    total_setlists: Option<i64>,
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
    songs_with_lyrics: Option<i64>,
    songs_without_lyrics: Option<i64>,
    active_users: Option<i64>,
    inactive_users: Option<i64>,
}

#[async_trait::async_trait]
pub trait MetricsRepository: Send + Sync {
    async fn get_user_metrics(&self, user_id: Uuid) -> Result<UserMetrics, ApiError>;
    async fn get_admin_metrics(&self) -> Result<AdminMetrics, ApiError>;
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
                (SELECT COUNT(*) FROM artists  WHERE user_id = $1) AS total_artists,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1) AS total_songs,
                (SELECT COUNT(*) FROM setlists WHERE user_id = $1) AS total_setlists,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND lyrics IS NOT NULL AND lyrics <> '') AS songs_with_lyrics,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND (lyrics IS NULL OR lyrics = ''))     AS songs_without_lyrics,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND tonality IS NOT NULL)                AS songs_with_tonality,
                (SELECT COUNT(*) FROM songs    WHERE user_id = $1 AND tempo IS NOT NULL)                   AS songs_with_tempo"
        )
        .bind(user_id)
        .fetch_one(&self.db);

        let genres_fut = sqlx::query_as::<_, GenreCount>(
            "SELECT genre::text AS genre, COUNT(*) AS count
             FROM songs
             WHERE user_id = $1 AND genre IS NOT NULL
             GROUP BY genre
             ORDER BY COUNT(*) DESC
             LIMIT 5",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let artists_fut = sqlx::query_as::<_, ArtistSongCount>(
            "SELECT a.name AS artist_name, COUNT(s.id) AS song_count
             FROM artists a
             LEFT JOIN songs s ON s.artist_id = a.id AND s.user_id = $1
             WHERE a.user_id = $1
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
            songs_with_lyrics: counts.songs_with_lyrics.unwrap_or(0),
            songs_without_lyrics: counts.songs_without_lyrics.unwrap_or(0),
            songs_with_tonality: counts.songs_with_tonality.unwrap_or(0),
            songs_with_tempo: counts.songs_with_tempo.unwrap_or(0),
            top_genres,
            top_artists_by_songs,
        })
    }

    async fn get_admin_metrics(&self) -> Result<AdminMetrics, ApiError> {
        let counts_fut = sqlx::query_as::<_, AdminCountsRow>(
            "SELECT
                (SELECT COUNT(*) FROM users)    AS total_users,
                (SELECT COUNT(*) FROM artists)  AS total_artists,
                (SELECT COUNT(*) FROM songs)    AS total_songs,
                (SELECT COUNT(*) FROM setlists) AS total_setlists,
                (SELECT COUNT(*) FROM songs WHERE lyrics IS NOT NULL AND lyrics <> '') AS songs_with_lyrics,
                (SELECT COUNT(*) FROM songs WHERE lyrics IS NULL OR lyrics = '')       AS songs_without_lyrics,
                (SELECT COUNT(*) FROM users WHERE status = 'active')                   AS active_users,
                (SELECT COUNT(*) FROM users WHERE status = 'inactive')                 AS inactive_users"
        )
        .fetch_one(&self.db);

        let genres_fut = sqlx::query_as::<_, GenreCount>(
            "SELECT genre::text AS genre, COUNT(*) AS count
             FROM songs
             WHERE genre IS NOT NULL
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
            songs_with_lyrics: counts.songs_with_lyrics.unwrap_or(0),
            songs_without_lyrics: counts.songs_without_lyrics.unwrap_or(0),
            active_users: counts.active_users.unwrap_or(0),
            inactive_users: counts.inactive_users.unwrap_or(0),
            top_genres,
            users_by_role,
        })
    }
}
