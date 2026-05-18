use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;

/// Metrics scoped to a single user.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserMetrics {
    pub total_artists: i64,
    pub total_songs: i64,
    pub total_setlists: i64,
    pub songs_with_lyrics: i64,
    pub songs_without_lyrics: i64,
    pub songs_with_tonality: i64,
    pub songs_with_tempo: i64,
    pub top_genres: Vec<GenreCount>,
    pub top_artists_by_songs: Vec<ArtistSongCount>,
}

/// Global metrics visible to admins only.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminMetrics {
    pub total_users: i64,
    pub total_artists: i64,
    pub total_songs: i64,
    pub total_setlists: i64,
    pub songs_with_lyrics: i64,
    pub songs_without_lyrics: i64,
    pub active_users: i64,
    pub inactive_users: i64,
    pub top_genres: Vec<GenreCount>,
    pub users_by_role: Vec<RoleCount>,
}

#[derive(Debug, Serialize, Deserialize, FromRow, ToSchema)]
pub struct GenreCount {
    pub genre: String,
    pub count: i64,
}

#[derive(Debug, Serialize, Deserialize, FromRow, ToSchema)]
pub struct ArtistSongCount {
    pub artist_name: String,
    pub song_count: i64,
}

#[derive(Debug, Serialize, Deserialize, FromRow, ToSchema)]
pub struct RoleCount {
    pub role: String,
    pub count: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum MetricsResponse {
    User(UserMetrics),
    Admin(AdminMetrics),
}
