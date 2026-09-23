pub mod connection;
pub mod repositories;

use repositories::{
    admin_repository::{AdminRepository, AdminRepositoryImpl},
    artist_repository::{ArtistRepository, ArtistRepositoryImpl},
    audit_repository::{AuditRepository, AuditRepositoryImpl},
    backup_repository::{BackupRepository, BackupRepositoryImpl},
    band_invite_repository::{BandInviteRepository, BandInviteRepositoryImpl},
    band_member_repository::{BandMemberRepository, BandMemberRepositoryImpl},
    band_repository::{BandRepository, BandRepositoryImpl},
    gig_repository::{GigRepository, GigRepositoryImpl},
    metrics_repository::{MetricsRepository, MetricsRepositoryImpl},
    notification_repository::{NotificationRepository, NotificationRepositoryImpl},
    quota_repository::{QuotaRepository, QuotaRepositoryImpl},
    setlist_repository::{SetlistRepository, SetlistRepositoryImpl},
    song_repository::{SongRepository, SongRepositoryImpl},
    user_preferences_repository::{UserPreferencesRepository, UserPreferencesRepositoryImpl},
    user_repository::{UserRepository, UserRepositoryImpl},
};
use sqlx::PgPool;
use std::{sync::Arc, time::Instant};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// When this process started — reported by `/status` as uptime.
    pub started_at: Instant,
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
    pub notification_repo: Arc<dyn NotificationRepository>,
    pub quota_repo: Arc<dyn QuotaRepository>,
    pub audit_repo: Arc<dyn AuditRepository>,
    pub admin_repo: Arc<dyn AdminRepository>,
}

impl AppState {
    /// Wires every repository to `pool`. The single place new
    /// repositories are registered — the server and the CLI binaries all
    /// build their state through here.
    pub fn new(pool: PgPool) -> Self {
        Self {
            started_at: Instant::now(),
            user_repo: Arc::new(UserRepositoryImpl::new(pool.clone())),
            user_prefs_repo: Arc::new(UserPreferencesRepositoryImpl::new(pool.clone())),
            artist_repo: Arc::new(ArtistRepositoryImpl::new(pool.clone())),
            song_repo: Arc::new(SongRepositoryImpl::new(pool.clone())),
            setlist_repo: Arc::new(SetlistRepositoryImpl::new(pool.clone())),
            gig_repo: Arc::new(GigRepositoryImpl::new(pool.clone())),
            metrics_repo: Arc::new(MetricsRepositoryImpl::new(pool.clone())),
            backup_repo: Arc::new(BackupRepositoryImpl::new(pool.clone())),
            band_repo: Arc::new(BandRepositoryImpl::new(pool.clone())),
            band_member_repo: Arc::new(BandMemberRepositoryImpl::new(pool.clone())),
            band_invite_repo: Arc::new(BandInviteRepositoryImpl::new(pool.clone())),
            notification_repo: Arc::new(NotificationRepositoryImpl::new(pool.clone())),
            quota_repo: Arc::new(QuotaRepositoryImpl::new(pool.clone())),
            audit_repo: Arc::new(AuditRepositoryImpl::new(pool.clone())),
            admin_repo: Arc::new(AdminRepositoryImpl::new(pool.clone())),
            db: pool,
        }
    }
}
