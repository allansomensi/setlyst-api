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
use sqlx::{PgConnection, PgPool, Postgres};
use uuid::Uuid;

const QUOTA_DEFAULTS_KEY: &str = "quota_defaults";

/// A quota check to run inside the transaction that inserts the counted
/// rows, so parallel requests can't all pass a count taken before any of
/// them inserted (check-then-insert).
///
/// Obtained from [`QuotaRepository::user_guard`] and friends, which
/// resolve the limits and fail fast when the quota is already spent; the
/// repository creating the rows then calls [`QuotaGuard::enforce`] as the
/// first statement of its transaction. `enforce` takes a transaction-scoped
/// advisory lock on `quota:<resource>:<scope>`, so creations counted
/// against the same scope run one after another until commit, and counts
/// again under it.
#[derive(Debug, Clone, Copy)]
pub struct QuotaGuard {
    limits: Option<QuotaLimits>,
    resource: QuotaResource,
    scope_id: Uuid,
    adding: i64,
}

impl QuotaGuard {
    /// A guard that never refuses (for callers exempt from quotas).
    pub fn unlimited(resource: QuotaResource, scope_id: Uuid) -> Self {
        Self {
            limits: None,
            resource,
            scope_id,
            adding: 0,
        }
    }

    /// Locks the scope and fails with `QUOTA_EXCEEDED` when adding the
    /// rows would go over the limit. Call it inside the transaction that
    /// inserts them, before inserting.
    pub async fn enforce(&self, conn: &mut PgConnection) -> Result<(), ApiError> {
        let Some(limits) = self.limits else {
            return Ok(());
        };
        if self.adding <= 0 {
            return Ok(());
        }
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('quota:' || $1 || ':' || $2::text, 0))",
        )
        .bind(self.resource.key())
        .bind(self.scope_id)
        .execute(&mut *conn)
        .await?;
        let limit = limits.get(self.resource);
        let used = count_in(&mut *conn, self.resource, self.scope_id).await?;
        if used + self.adding > limit {
            tracing::warn!(scope_id = %self.scope_id, resource = self.resource.key(), used, adding = self.adding, limit, "Quota exceeded");
            return Err(ApiError::quota_exceeded(self.resource.key(), limit));
        }
        Ok(())
    }

    /// Enforces every guard, in order.
    pub async fn enforce_all(
        guards: &[QuotaGuard],
        conn: &mut PgConnection,
    ) -> Result<(), ApiError> {
        for guard in guards {
            guard.enforce(&mut *conn).await?;
        }
        Ok(())
    }
}

/// How many of `resource` the scope (a user, band or setlist) holds.
async fn count_in<'e, E>(
    executor: E,
    resource: QuotaResource,
    scope_id: Uuid,
) -> Result<i64, ApiError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
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
        QuotaResource::BandMemberships => "SELECT COUNT(*) FROM band_members WHERE user_id = $1",
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
        .fetch_one(executor)
        .await?;
    Ok(count)
}

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

    /// [`ensure_user`](Self::ensure_user) now (fail fast), plus a
    /// [`QuotaGuard`] to enforce it again, atomically, in the transaction
    /// that creates the rows.
    async fn user_guard(
        &self,
        user_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError>;
    /// Same, for a per-band resource.
    async fn band_guard(
        &self,
        band_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError>;
    /// Same, for the items of one setlist.
    async fn setlist_items_guard(
        &self,
        setlist_id: Uuid,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError>;
}

pub struct QuotaRepositoryImpl {
    pub db: PgPool,
}

impl QuotaRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    async fn count(&self, resource: QuotaResource, scope_id: Uuid) -> Result<i64, ApiError> {
        count_in(&self.db, resource, scope_id).await
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
        self.user_guard(user_id, resource, adding).await.map(|_| ())
    }

    async fn ensure_band(
        &self,
        band_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<(), ApiError> {
        self.band_guard(band_id, resource, adding).await.map(|_| ())
    }

    async fn ensure_setlist_items(&self, setlist_id: Uuid, adding: i64) -> Result<(), ApiError> {
        self.setlist_items_guard(setlist_id, adding)
            .await
            .map(|_| ())
    }

    async fn user_guard(
        &self,
        user_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError> {
        let limits = self.effective_limits(user_id).await?;
        self.check(limits, resource, user_id, adding).await?;
        Ok(QuotaGuard {
            limits,
            resource,
            scope_id: user_id,
            adding,
        })
    }

    async fn band_guard(
        &self,
        band_id: Uuid,
        resource: QuotaResource,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError> {
        let limits = match self.band_owner(band_id).await? {
            Some(owner) => self.effective_limits(owner).await?,
            None => Some(self.get_defaults().await?),
        };
        self.check(limits, resource, band_id, adding).await?;
        Ok(QuotaGuard {
            limits,
            resource,
            scope_id: band_id,
            adding,
        })
    }

    async fn setlist_items_guard(
        &self,
        setlist_id: Uuid,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError> {
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
            .await?;
        Ok(QuotaGuard {
            limits,
            resource: QuotaResource::SetlistItems,
            scope_id: setlist_id,
            adding,
        })
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
