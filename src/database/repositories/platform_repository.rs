use crate::{errors::api_error::ApiError, models::platform::PlatformSettings};
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use std::{
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tracing::error;
use uuid::Uuid;

/// `platform_settings` key of the platform switches.
pub const PLATFORM_SETTINGS_KEY: &str = "platform";

/// How long a read of the settings is reused. Maintenance mode is checked
/// on every authenticated request, so it is cached in-process; another
/// instance sees a change within this delay (the one that made it, at
/// once).
pub const PLATFORM_SETTINGS_TTL: Duration = Duration::from_secs(5);

#[async_trait::async_trait]
pub trait PlatformRepository: Send + Sync {
    /// The current settings (cached for [`PLATFORM_SETTINGS_TTL`]).
    async fn get(&self) -> Result<PlatformSettings, ApiError>;
    /// The stored settings, bypassing the cache.
    async fn load(&self) -> Result<PlatformSettings, ApiError>;
    async fn save(&self, settings: &PlatformSettings, actor_id: Uuid) -> Result<(), ApiError>;
}

pub struct PlatformRepositoryImpl {
    db: PgPool,
    cache: RwLock<Option<(Instant, PlatformSettings)>>,
    /// Bumped by every save: a read that started before it must not put
    /// the older value it fetched back in the cache.
    generation: AtomicU64,
}

impl PlatformRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            cache: RwLock::new(None),
            generation: AtomicU64::new(0),
        }
    }

    fn cached(&self) -> Option<PlatformSettings> {
        let cache = self.cache.read().ok()?;
        cache
            .as_ref()
            .filter(|(at, _)| at.elapsed() < PLATFORM_SETTINGS_TTL)
            .map(|(_, settings)| settings.clone())
    }

    /// Caches `settings`, read when the generation was `generation`
    /// (skipped when a save happened since).
    fn remember(&self, settings: &PlatformSettings, generation: u64) {
        if let Ok(mut cache) = self.cache.write()
            && self.generation.load(Ordering::SeqCst) == generation
        {
            *cache = Some((Instant::now(), settings.clone()));
        }
    }
}

#[async_trait::async_trait]
impl PlatformRepository for PlatformRepositoryImpl {
    async fn get(&self) -> Result<PlatformSettings, ApiError> {
        if let Some(settings) = self.cached() {
            return Ok(settings);
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let settings = self.load().await?;
        self.remember(&settings, generation);
        Ok(settings)
    }

    async fn load(&self) -> Result<PlatformSettings, ApiError> {
        let stored: Option<Value> =
            sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = $1")
                .bind(PLATFORM_SETTINGS_KEY)
                .fetch_optional(&self.db)
                .await?;
        Ok(match stored {
            None => PlatformSettings::default(),
            Some(value) => serde_json::from_value(value).unwrap_or_else(|e| {
                // Falls back to an open platform: loudly, since every switch
                // (maintenance, closed sign-ups, blocked domains) is lost.
                error!(error = %e, "Stored platform settings are unreadable; using the defaults");
                PlatformSettings::default()
            }),
        })
    }

    async fn save(&self, settings: &PlatformSettings, actor_id: Uuid) -> Result<(), ApiError> {
        let value =
            serde_json::to_value(settings).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        sqlx::query(
            "INSERT INTO platform_settings (key, value, updated_at, updated_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = $3, updated_by = $4",
        )
        .bind(PLATFORM_SETTINGS_KEY)
        .bind(value)
        .bind(Utc::now().naive_utc())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        // Under the cache lock, so a read finishing now can't slip its
        // older value in between the bump and the store.
        if let Ok(mut cache) = self.cache.write() {
            self.generation.fetch_add(1, Ordering::SeqCst);
            *cache = Some((Instant::now(), settings.clone()));
        }
        Ok(())
    }
}
