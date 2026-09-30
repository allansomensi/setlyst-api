use crate::{
    errors::api_error::ApiError,
    models::{
        billing::{AccessTier, BILLING_SETTINGS_KEY, BillingSettings},
        quota::{
            QuotaLimits, QuotaOverrides, QuotaReport, QuotaResource, QuotaUsageItem,
            UserQuotaSettings,
        },
        user::Role,
    },
};
use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Postgres, Row};
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
    /// The limits the guard enforces (`None` = exempt), so a caller that
    /// needs them for something else doesn't resolve them a second time.
    pub fn limits(&self) -> Option<QuotaLimits> {
        self.limits
    }

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
        lock_scope(&mut *conn, self.resource, self.scope_id).await?;
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

/// Takes the transaction-scoped advisory lock creations of `resource` in
/// the scope are serialized under (`quota:<resource>:<scope>`), for code
/// that counts and inserts on its own (restores, imports, copies) and must
/// see the same count a concurrent [`QuotaGuard::enforce`] sees.
pub async fn lock_scope(
    conn: &mut PgConnection,
    resource: QuotaResource,
    scope_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('quota:' || $1 || ':' || $2::text, 0))",
    )
    .bind(resource.key())
    .bind(scope_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
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
    let count: i64 = sqlx::query_scalar(count_sql(resource))
        .bind(scope_id)
        .fetch_one(executor)
        .await?;
    Ok(count)
}

/// The statement counting `resource` in scope `$1`. Each arm is a literal
/// so sqlx accepts it (no runtime-built SQL).
const fn count_sql(resource: QuotaResource) -> &'static str {
    match resource {
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
    }
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
    /// (staff, or the per-user `unlimited` flag): by [`AccessTier`], the
    /// plan's limits, `QuotaLimits::UNVERIFIED`, `QuotaLimits::FREE` or the
    /// platform defaults (the beta), then the per-user overrides.
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
    /// [`setlist_items_guard`](Self::setlist_items_guard) for a setlist
    /// that may be a band's repertoire, whose songs are bounded by the
    /// band's song quota instead: `None` then, the guard otherwise.
    /// `NotFound` for an unknown setlist.
    async fn setlist_item_room(
        &self,
        setlist_id: Uuid,
        adding: i64,
    ) -> Result<Option<QuotaGuard>, ApiError>;
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

    /// The limits of `user_id` together with their overrides (see
    /// [`QuotaRepository::effective_limits`]). One round trip reads the
    /// account, its quota settings and both platform settings rows; only
    /// while plans are enforced is the plan in effect read as well.
    async fn resolve_limits(
        &self,
        user_id: Uuid,
    ) -> Result<(Option<QuotaLimits>, QuotaOverrides), ApiError> {
        let row = sqlx::query(
            "SELECT u.role, (u.email_verified_at IS NOT NULL AND u.email IS NOT NULL) AS email_verified,
                    q.unlimited, q.overrides,
                    (SELECT value FROM platform_settings WHERE key = $2) AS billing,
                    (SELECT value FROM platform_settings WHERE key = $3) AS defaults
             FROM users u
             LEFT JOIN user_quotas q ON q.user_id = u.id
             WHERE u.id = $1",
        )
        .bind(user_id)
        .bind(BILLING_SETTINGS_KEY)
        .bind(QUOTA_DEFAULTS_KEY)
        .fetch_optional(&self.db)
        .await?;
        let Some(row) = row else {
            // An unknown account: nothing is exempt, the beta defaults
            // apply (as before, so callers keep failing on the insert's
            // foreign key rather than here).
            let defaults = self.get_defaults().await?;
            return Ok((Some(defaults), QuotaOverrides::default()));
        };
        let role: Role = row.try_get("role")?;
        let email_verified: bool = row.try_get("email_verified")?;
        let unlimited: Option<bool> = row.try_get("unlimited")?;
        let overrides: QuotaOverrides = row
            .try_get::<Option<Value>, _>("overrides")?
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let billing: BillingSettings = row
            .try_get::<Option<Value>, _>("billing")?
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let defaults: QuotaLimits = row
            .try_get::<Option<Value>, _>("defaults")?
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();

        // Staff already have access to everything.
        if role.is_staff() || unlimited.unwrap_or(false) {
            return Ok((None, overrides));
        }

        // See `AccessTier`: the plan's limits while plans are enforced,
        // the platform defaults in the beta, and the small built-in tiers
        // for unverified accounts and accounts without a plan. Per-user
        // overrides apply on top either way.
        let plan = if billing.enforced {
            super::billing_repository::load_effective_plan(&self.db, user_id).await?
        } else {
            None
        };
        let tier = AccessTier::resolve(false, billing.enforced, plan.is_some(), email_verified);
        let base = match (tier, plan) {
            (AccessTier::Plan, Some(plan)) => plan.limits,
            (AccessTier::Unverified, _) => QuotaLimits::UNVERIFIED,
            (AccessTier::Free, _) => QuotaLimits::FREE,
            _ => defaults,
        };
        Ok((Some(base.with_overrides(&overrides)), overrides))
    }

    /// The setlist's owner, band and repertoire flag, and the band's
    /// owner when it has one — whoever's limits its items count against.
    async fn setlist_scope(
        &self,
        setlist_id: Uuid,
    ) -> Result<(Uuid, Option<Uuid>, bool, Option<Uuid>), ApiError> {
        sqlx::query_as(
            "SELECT s.user_id, s.band_id, s.is_repertoire,
                    (SELECT bm.user_id FROM band_members bm
                     WHERE bm.band_id = s.band_id AND bm.role = 'owner' LIMIT 1)
             FROM setlists s WHERE s.id = $1",
        )
        .bind(setlist_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)
    }

    async fn setlist_items_guard_for(
        &self,
        setlist_id: Uuid,
        responsible: Uuid,
        adding: i64,
    ) -> Result<QuotaGuard, ApiError> {
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
        Ok(self.resolve_limits(user_id).await?.0)
    }

    async fn report(&self, user_id: Uuid) -> Result<QuotaReport, ApiError> {
        // The limits (with the overrides they were built from) and every
        // per-user count, in one statement each.
        let limits_and_overrides = self.resolve_limits(user_id);
        let counts = async {
            let selects: Vec<String> = QuotaResource::PER_USER
                .iter()
                .map(|resource| format!("({})", count_sql(*resource)))
                .collect();
            // Every fragment is a literal of this file; only `$1` is bound.
            let sql = format!("SELECT {}", selects.join(", "));
            let row = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(user_id)
                .fetch_one(&self.db)
                .await?;
            let mut used = Vec::with_capacity(QuotaResource::PER_USER.len());
            for index in 0..QuotaResource::PER_USER.len() {
                used.push(row.try_get::<i64, _>(index)?);
            }
            Ok::<_, ApiError>(used)
        };
        let ((limits, overrides), used) = tokio::try_join!(limits_and_overrides, counts)?;
        let overrides = serde_json::to_value(&overrides).unwrap_or_default();

        let mut items = Vec::with_capacity(QuotaResource::ALL.len());
        for resource in QuotaResource::ALL {
            let used = QuotaResource::PER_USER
                .iter()
                .position(|r| *r == resource)
                .map(|index| used[index]);

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
        let (owner_id, _, _, band_owner) = self.setlist_scope(setlist_id).await?;
        self.setlist_items_guard_for(setlist_id, band_owner.unwrap_or(owner_id), adding)
            .await
    }

    async fn setlist_item_room(
        &self,
        setlist_id: Uuid,
        adding: i64,
    ) -> Result<Option<QuotaGuard>, ApiError> {
        let (owner_id, _, is_repertoire, band_owner) = self.setlist_scope(setlist_id).await?;
        if is_repertoire {
            return Ok(None);
        }
        self.setlist_items_guard_for(setlist_id, band_owner.unwrap_or(owner_id), adding)
            .await
            .map(Some)
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
