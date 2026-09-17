use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;

/// Metrics scoped to a single user.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserMetrics {
    pub total_artists: i64,
    pub total_songs: i64,
    pub total_setlists: i64,
    pub total_bands: i64,
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
    pub total_bands: i64,
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

/// One day's count in a time series (e.g. "5 songs added on 2026-09-10").
/// Every day in the requested range is present, even with `count: 0` — the
/// frontend charts rely on there being no gaps.
#[derive(Debug, Serialize, Deserialize, FromRow, ToSchema)]
pub struct TimeseriesPoint {
    pub date: chrono::NaiveDate,
    pub count: i64,
}

/// Time-series activity scoped to a single user (their own personal
/// content only — mirrors the scoping used by [`UserMetrics`]).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserTimeseries {
    pub songs_created: Vec<TimeseriesPoint>,
    pub setlists_created: Vec<TimeseriesPoint>,
    pub gigs_created: Vec<TimeseriesPoint>,
}

/// Platform-wide time-series activity, visible to admins only.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AdminTimeseries {
    pub users_registered: Vec<TimeseriesPoint>,
    pub songs_created: Vec<TimeseriesPoint>,
    pub setlists_created: Vec<TimeseriesPoint>,
    pub bands_created: Vec<TimeseriesPoint>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum TimeseriesResponse {
    User(UserTimeseries),
    Admin(AdminTimeseries),
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimeseriesQuery {
    /// Trailing window size in days. Defaults to 30, clamped to [1, 365].
    pub days: Option<i64>,
}

impl TimeseriesQuery {
    /// Clamped, default-applied day count — never trust the raw query
    /// value directly, since an unbounded range would make the
    /// `generate_series` query unbounded too.
    pub fn days(&self) -> i64 {
        self.days.unwrap_or(30).clamp(1, 365)
    }
}
