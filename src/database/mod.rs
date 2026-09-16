pub mod connection;
pub mod repositories;

use repositories::{
    artist_repository::ArtistRepository, backup_repository::BackupRepository,
    band_invite_repository::BandInviteRepository, band_member_repository::BandMemberRepository,
    band_repository::BandRepository, gig_repository::GigRepository,
    metrics_repository::MetricsRepository, setlist_repository::SetlistRepository,
    song_repository::SongRepository, user_preferences_repository::UserPreferencesRepository,
    user_repository::UserRepository,
};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub user_repo: Arc<dyn UserRepository>,
    pub user_prefs_repo: Arc<dyn UserPreferencesRepository>,
    pub artist_repo: Arc<dyn ArtistRepository>,
    pub song_repo: Arc<dyn SongRepository>,
    pub setlist_repo: Arc<dyn SetlistRepository>,
    pub gig_repo: Arc<dyn GigRepository>,
    pub metrics_repo: Arc<dyn MetricsRepository>,
    pub backup_repo: Arc<dyn BackupRepository>,
    pub band_repo: Arc<dyn BandRepository>,
    pub band_member_repo: Arc<dyn BandMemberRepository>,
    pub band_invite_repo: Arc<dyn BandInviteRepository>,
}
