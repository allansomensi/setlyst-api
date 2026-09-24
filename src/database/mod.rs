pub mod connection;
pub mod repositories;

use crate::{
    config::Config,
    moderation::{DefaultModerationService, ModerationService},
    payments::Payments,
    services::google::{GoogleJwksVerifier, GoogleTokenVerifier},
};
use repositories::{
    admin_repository::{AdminRepository, AdminRepositoryImpl},
    announcement_repository::{AnnouncementRepository, AnnouncementRepositoryImpl},
    artist_repository::{ArtistRepository, ArtistRepositoryImpl},
    audit_repository::{AuditRepository, AuditRepositoryImpl},
    backup_repository::{BackupRepository, BackupRepositoryImpl},
    band_invite_repository::{BandInviteRepository, BandInviteRepositoryImpl},
    band_member_repository::{BandMemberRepository, BandMemberRepositoryImpl},
    band_note_repository::{BandNoteRepository, BandNoteRepositoryImpl},
    band_repository::{BandRepository, BandRepositoryImpl},
    billing_repository::{BillingRepository, BillingRepositoryImpl},
    gig_repository::{GigRepository, GigRepositoryImpl},
    metrics_repository::{MetricsRepository, MetricsRepositoryImpl},
    moderation_repository::{ModerationRepository, ModerationRepositoryImpl},
    notification_repository::{NotificationRepository, NotificationRepositoryImpl},
    pin_repository::{PinRepository, PinRepositoryImpl},
    quota_repository::{QuotaRepository, QuotaRepositoryImpl},
    release_note_repository::{ReleaseNoteRepository, ReleaseNoteRepositoryImpl},
    security_repository::{SecurityRepository, SecurityRepositoryImpl},
    setlist_repository::{SetlistRepository, SetlistRepositoryImpl},
    song_repository::{SongRepository, SongRepositoryImpl},
    suggestion_repository::{SuggestionRepository, SuggestionRepositoryImpl},
    tour_repository::{TourRepository, TourRepositoryImpl},
    trash_repository::{TrashRepository, TrashRepositoryImpl},
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
    pub security_repo: Arc<dyn SecurityRepository>,
    pub billing_repo: Arc<dyn BillingRepository>,
    pub announcement_repo: Arc<dyn AnnouncementRepository>,
    pub release_note_repo: Arc<dyn ReleaseNoteRepository>,
    pub moderation_repo: Arc<dyn ModerationRepository>,
    pub tour_repo: Arc<dyn TourRepository>,
    pub trash_repo: Arc<dyn TrashRepository>,
    pub suggestion_repo: Arc<dyn SuggestionRepository>,
    pub band_note_repo: Arc<dyn BandNoteRepository>,
    pub pin_repo: Arc<dyn PinRepository>,
    /// Verifies Google ID tokens (replaced by a fake in tests).
    pub google_verifier: Arc<dyn GoogleTokenVerifier>,
    /// Automatic moderation checks (replaced in tests).
    pub moderation: Arc<dyn ModerationService>,
    /// Card payments; `None` when not configured (replaced in tests).
    pub payments: Option<Payments>,
}

impl AppState {
    /// Wires every repository to `pool`. The single place new
    /// repositories are registered — the server and the CLI binaries all
    /// build their state through here.
    pub fn new(pool: PgPool) -> Self {
        Self::with_services(
            pool.clone(),
            Arc::new(GoogleJwksVerifier::new()),
            Arc::new(DefaultModerationService::new().with_budget(pool)),
        )
    }

    /// Like [`AppState::new`], with the external services injected (tests
    /// use fakes that never touch the network).
    pub fn with_services(
        pool: PgPool,
        google_verifier: Arc<dyn GoogleTokenVerifier>,
        moderation: Arc<dyn ModerationService>,
    ) -> Self {
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
            security_repo: Arc::new(SecurityRepositoryImpl::new(pool.clone())),
            billing_repo: Arc::new(BillingRepositoryImpl::new(pool.clone())),
            announcement_repo: Arc::new(AnnouncementRepositoryImpl::new(pool.clone())),
            release_note_repo: Arc::new(ReleaseNoteRepositoryImpl::new(pool.clone())),
            moderation_repo: Arc::new(ModerationRepositoryImpl::new(pool.clone())),
            tour_repo: Arc::new(TourRepositoryImpl::new(pool.clone())),
            trash_repo: Arc::new(TrashRepositoryImpl::new(pool.clone())),
            suggestion_repo: Arc::new(SuggestionRepositoryImpl::new(pool.clone())),
            band_note_repo: Arc::new(BandNoteRepositoryImpl::new(pool.clone())),
            pin_repo: Arc::new(PinRepositoryImpl::new(pool.clone())),
            google_verifier,
            moderation,
            payments: Config::try_get().and_then(Payments::from_config),
            db: pool,
        }
    }

    /// Replaces the payment integration (tests use a fake gateway).
    pub fn with_payments(mut self, payments: Option<Payments>) -> Self {
        self.payments = payments;
        self
    }
}
