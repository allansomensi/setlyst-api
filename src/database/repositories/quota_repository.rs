use crate::{
    errors::api_error::ApiError,
    models::{
        quota::{
            QuotaLimits, QuotaOverrides, QuotaReport, QuotaResource, QuotaUsageItem,
            UserQuotaSettings,
        },
        user::Role,
    },
};
use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

const QUOTA_DEFAULTS_KEY: &str = "quota_defaults";

#[async_trait::async_trait]
pub trait QuotaRepository: Send + Sync {
    /// The platform-wide defaults (built-in values until an admin saves
    /// their own).
    async fn get_defaults(&self) -> Result<QuotaLimits, ApiError>;
    async fn set_defaults(&self, limits: &QuotaLimits, actor_id: Uuid) -> Result<(), ApiError>;
    async fn get_user_settings(&self, user_id: Uuid) -> Result<UserQuotaSettings, ApiError>;
    async fn set_user_settings(
        &self,
        user_id: Uuid,
        overrides: &QuotaOverrides,
        unlimited: bool,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;
    /// The limits that apply to `user_id`, or `None` when they are exempt
    /// (admins, or the per-user `unlimited` flag): the plan's limits when
    /// plans are enforced (platform defaults without a plan), the platform
    /// defaults otherwise, then the per-user overrides.
    async fn effective_limits(&self, user_id: Uuid) -> Result<Option<QuotaLimits>, ApiError>;
    /// Current usage for every per-user resource plus the effective
    /// per-band/per-setlist limits.
    async fn report(&self, user_id: Uuid) -> Result<QuotaReport, ApiError>;

    /// Fails with `QUOTA_EXCEEDED` if `user_id` creating `adding` more of a
    /// per-user `resource` would go over their limit.
    async fn ensure_user(
        &self,
        user_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<(), ApiError>;
    /// Same, for a per-band resource — limits come from the band's owner.
    async fn ensure_band(
        &self,
        band_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<(), ApiError>;
    /// Same, for the items of one setlist — limits come from the
    /// setlist's owner (or its band's owner).
    async fn ensure_setlist_items(&self, setlist_id: Uuid, adding: i64) -> Result<(), ApiError>;
    /// Fails if giving `user_id` the tags in `tags` would take their
    /// personal tag vocabulary over the limit.
    async fn ensure_tags(&self, user_id: Uuid, tags: &[String]) -> Result<(), ApiError>;
}

pub struct QuotaRepositoryImpl {
    pub db: PgPool,
}

impl QuotaRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    async fn count(&self, resource: QuotaResource, scope_id: Uuid) -> Result<i64, ApiError> {
        // Each arm is a literal so sqlx accepts it (no runtime-built SQL).
        let query = match resource {
            QuotaResource::Songs => {
                "SELECT COUNT(*) FROM songs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL"
            }
            QuotaResource::Artists => {
                "SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL"
            }
            QuotaResource::Setlists => {
                "SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL"
            }
            QuotaResource::Gigs => {
                "SELECT COUNT(*) FROM gigs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL"
            }
            QuotaResource::Tags => {
                "SELECT COUNT(DISTINCT st.tag) FROM song_tags st
                 INNER JOIN songs s ON s.id = st.song_id
                 WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL"
            }
            QuotaResource::BandsOwned => {
                "SELECT COUNT(*) FROM band_members WHERE user_id = $1 AND role = 'owner'"
            }
            QuotaResource::BandMemberships => {
                "SELECT COUNT(*) FROM band_members WHERE user_id = $1"
            }
            QuotaResource::BandMembers => "SELECT COUNT(*) FROM band_members WHERE band_id = $1",
            QuotaResource::BandSetlists => {
                "SELECT COUNT(*) FROM setlists WHERE band_id = $1 AND deleted_at IS NULL AND NOT is_repertoire"
            }
            QuotaResource::BandGigs => {
                "SELECT COUNT(*) FROM gigs WHERE band_id = $1 AND deleted_at IS NULL"
            }
            QuotaResource::BandSongs => {
                "SELECT COUNT(*) FROM songs WHERE band_id = $1 AND deleted_at IS NULL"
            }
            QuotaResource::SetlistItems => {
                "SELECT (SELECT COUNT(*) FROM setlist_songs ss
                         INNER JOIN songs s ON s.id = ss.song_id
                         WHERE ss.setlist_id = $1 AND s.deleted_at IS NULL)
                      + (SELECT COUNT(*) FROM setlist_markers WHERE setlist_id = $1)"
            }
            QuotaResource::Tours => {
                "SELECT COUNT(*) FROM tours WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL"
            }
            QuotaResource::BandTours => {
                "SELECT COUNT(*) FROM tours WHERE band_id = $1 AND deleted_at IS NULL"
            }
        };

        let count: i64 = sqlx::query_scalar(query)
            .bind(scope_id)
            .fetch_one(&self.db)
            .await?;
        Ok(count)
    }

    async fn check(
        &self,
        limits: Option<QuotaLimits>,
        resource: QuotaResource,
        scope_id: Uuid,
        adding: i64,
    ) -> Result<(), ApiError> {
        let Some(limits) = limits else {
            return Ok(());
        };
        if adding <= 0 {
            return Ok(());
        }

        let limit = limits.get(resource);
        let used = self.count(resource, scope_id).await?;

        if used + adding > limit {
            tracing::warn!(%scope_id, resource = resource.key(), used, adding, limit, "Quota exceeded");
            return Err(ApiError::quota_exceeded(resource.key(), limit));
        }
        Ok(())
    }

    async fn band_owner(&self, band_id: Uuid) -> Result<Option<Uuid>, ApiError> {
        let owner = sqlx::query_scalar(
            "SELECT user_id FROM band_members WHERE band_id = $1 AND role = 'owner' LIMIT 1",
        )
        .bind(band_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(owner)
    }
}

#[async_trait::async_trait]
impl QuotaRepository for QuotaRepositoryImpl {
    async fn get_defaults(&self) -> Result<QuotaLimits, ApiError> {
        let stored: Option<Value> =
            sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = $1")
                .bind(QUOTA_DEFAULTS_KEY)
                .fetch_optional(&self.db)
                .await?;

        // `QuotaLimits` is `#[serde(default)]`, so a stored object missing
        // a newer resource still deserializes, with the built-in value.
        Ok(stored
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default())
    }

    async fn set_defaults(&self, limits: &QuotaLimits, actor_id: Uuid) -> Result<(), ApiError> {
        let value =
            serde_json::to_value(limits).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        sqlx::query(
            "INSERT INTO platform_settings (key, value, updated_at, updated_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = $3, updated_by = $4",
        )
        .bind(QUOTA_DEFAULTS_KEY)
        .bind(value)
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn get_user_settings(&self, user_id: Uuid) -> Result<UserQuotaSettings, ApiError> {
        let row: Option<(Value, bool, NaiveDateTime, Option<String>)> = sqlx::query_as(
            "SELECT q.overrides, q.unlimited, q.updated_at,
                    (SELECT u.username FROM users u WHERE u.id = q.updated_by)
             FROM user_quotas q WHERE q.user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        Ok(match row {
            Some((overrides, unlimited, updated_at, updated_by_username)) => UserQuotaSettings {
                overrides: serde_json::from_value(overrides).unwrap_or_default(),
                unlimited,
                updated_at: Some(updated_at),
                updated_by_username,
            },
            None => UserQuotaSettings::default(),
        })
    }

    async fn set_user_settings(
        &self,
        user_id: Uuid,
        overrides: &QuotaOverrides,
        unlimited: bool,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let value =
            serde_json::to_value(overrides).map_err(|e| ApiError::BadRequest(e.to_string()))?;
        sqlx::query(
            "INSERT INTO user_quotas (user_id, overrides, unlimited, updated_at, updated_by)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO UPDATE SET overrides = $2, unlimited = $3, updated_at = $4, updated_by = $5",
        )
        .bind(user_id)
        .bind(value)
        .bind(unlimited)
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn effective_limits(&self, user_id: Uuid) -> Result<Option<QuotaLimits>, ApiError> {
        let role: Option<Role> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?;

        if role == Some(Role::Admin) {
            return Ok(None);
        }

        let settings = self.get_user_settings(user_id).await?;
        if settings.unlimited {
            return Ok(None);
        }

        // With plans enforced, the plan's limits replace the platform
        // defaults (accounts without a plan keep the defaults); per-user
        // overrides apply on top either way.
        let billing = super::billing_repository::load_settings(&self.db).await?;
        let base = if billing.enforced {
            match super::billing_repository::load_effective_plan(&self.db, user_id).await? {
                Some(plan) => plan.limits,
                None => self.get_defaults().await?,
            }
        } else {
            self.get_defaults().await?
        };
        Ok(Some(base.with_overrides(&settings.overrides)))
    }

    async fn report(&self, user_id: Uuid) -> Result<QuotaReport, ApiError> {
        let limits = self.effective_limits(user_id).await?;
        let settings = self.get_user_settings(user_id).await?;
        let overrides = serde_json::to_value(&settings.overrides).unwrap_or_default();

        let mut items = Vec::with_capacity(QuotaResource::ALL.len());
        for resource in QuotaResource::ALL {
            let used = if QuotaResource::PER_USER.contains(&resource) {
                Some(self.count(resource, user_id).await?)
            } else {
                None
            };

            items.push(QuotaUsageItem {
                resource,
                used,
                limit: limits.map(|l| l.get(resource)),
                overridden: overrides
                    .get(resource.key())
                    .is_some_and(|value| !value.is_null()),
            });
        }

        Ok(QuotaReport {
            unlimited: limits.is_none(),
            items,
        })
    }

    async fn ensure_user(
        &self,
        user_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<(), ApiError> {
        let limits = self.effective_limits(user_id).await?;
        self.check(limits, resource, user_id, adding).await
    }

    async fn ensure_band(
        &self,
        band_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<(), ApiError> {
        let limits = match self.band_owner(band_id).await? {
            Some(owner) => self.effective_limits(owner).await?,
            None => Some(self.get_defaults().await?),
        };
        self.check(limits, resource, band_id, adding).await
    }

    async fn ensure_setlist_items(&self, setlist_id: Uuid, adding: i64) -> Result<(), ApiError> {
        let row: Option<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT user_id, band_id FROM setlists WHERE id = $1")
                .bind(setlist_id)
                .fetch_optional(&self.db)
                .await?;

        let Some((owner_id, band_id)) = row else {
            return Err(ApiError::NotFound);
        };

        let responsible = match band_id {
            Some(band_id) => self.band_owner(band_id).await?.unwrap_or(owner_id),
            None => owner_id,
        };

        let limits = self.effective_limits(responsible).await?;
        self.check(limits, QuotaResource::SetlistItems, setlist_id, adding)
            .await
    }

    async fn ensure_tags(&self, user_id: Uuid, tags: &[String]) -> Result<(), ApiError> {
        if tags.is_empty() {
            return Ok(());
        }

        let limits = self.effective_limits(user_id).await?;
        if limits.is_none() {
            return Ok(());
        }

        // Only tags the user doesn't already use count as new.
        let new_tags: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM UNNEST($2::text[]) AS t(tag)
             WHERE NOT EXISTS (
                SELECT 1 FROM song_tags st
                INNER JOIN songs s ON s.id = st.song_id
                WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL AND st.tag = t.tag
             )",
        )
        .bind(user_id)
        .bind(tags)
        .fetch_one(&self.db)
        .await?;

        self.check(limits, QuotaResource::Tags, user_id, new_tags)
            .await
    }
}
