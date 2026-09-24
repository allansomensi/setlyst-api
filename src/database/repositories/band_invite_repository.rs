use crate::{
    errors::api_error::ApiError,
    models::band::{BandInvite, BandRole, CreateBandInvitePayload},
    utils::invite_code::generate_invite_code,
};
use chrono::{NaiveDateTime, Utc};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait BandInviteRepository: Send + Sync {
    /// Creates an invite. Under the band row lock, the creator's current
    /// role is re-checked (`admin`+ and above the invite's role:
    /// `Forbidden` otherwise, e.g. after a concurrent demotion) and the
    /// band's usable invites are capped at [`MAX_ACTIVE_INVITES_PER_BAND`].
    async fn create(
        &self,
        band_id: Uuid,
        created_by: Uuid,
        payload: &CreateBandInvitePayload,
    ) -> Result<BandInvite, ApiError>;

    /// Lists every invite for a band, most recent first (including
    /// expired/revoked ones, so admins can audit the band's invite history).
    async fn list(&self, band_id: Uuid) -> Result<Vec<BandInvite>, ApiError>;

    /// Looks up an invite by its public code, regardless of validity.
    async fn find_by_code(&self, code: &str) -> Result<Option<BandInvite>, ApiError>;

    async fn revoke(&self, id: Uuid, band_id: Uuid) -> Result<(), ApiError>;

    async fn increment_uses(&self, id: Uuid) -> Result<(), ApiError>;

    /// Redeems an invite for `user_id` atomically: the invite row is
    /// locked, re-validated (not revoked, expired or exhausted), the
    /// band's member count and the user's membership count are checked
    /// against `limits` (under locks on the band and the user, so
    /// concurrent joins can't overshoot them), the membership is created
    /// and the use counted in one transaction — so two people racing for
    /// the last use of a single-use invite can't both get in. Returns
    /// `(band_id, role)`.
    async fn redeem(
        &self,
        code: &str,
        user_id: Uuid,
        limits: InviteLimits,
    ) -> Result<(Uuid, BandRole), ApiError>;
}

/// Invites expire after this many hours unless the creator chose otherwise.
pub const DEFAULT_INVITE_EXPIRY_HOURS: i64 = 7 * 24;

/// Usable (not revoked, expired or used up) invites a band may have at
/// once (`QUOTA_EXCEEDED`, resource `band_invites`).
pub const MAX_ACTIVE_INVITES_PER_BAND: i64 = 50;

/// Limits enforced while redeeming an invite (`None` = unlimited).
#[derive(Debug, Clone, Copy, Default)]
pub struct InviteLimits {
    /// The band's member limit (from its owner's quota).
    pub band_members: Option<i64>,
    /// How many bands the joining user may belong to.
    pub band_memberships: Option<i64>,
}

pub struct BandInviteRepositoryImpl {
    pub db: PgPool,
}

impl BandInviteRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BandInviteRepository for BandInviteRepositoryImpl {
    async fn create(
        &self,
        band_id: Uuid,
        created_by: Uuid,
        payload: &CreateBandInvitePayload,
    ) -> Result<BandInvite, ApiError> {
        let role = payload.role.unwrap_or(BandRole::Member);
        // Invites expire after a week unless asked otherwise, so a link
        // pasted somewhere public doesn't stay usable forever.
        let hours = payload
            .expires_in_hours
            .unwrap_or(DEFAULT_INVITE_EXPIRY_HOURS);
        let expires_at: Option<NaiveDateTime> =
            Some(Utc::now().naive_utc() + chrono::Duration::hours(hours));

        let mut code = generate_invite_code();
        for attempt in 0..5 {
            let taken = sqlx::query("SELECT id FROM band_invites WHERE code = $1;")
                .bind(&code)
                .fetch_optional(&self.db)
                .await?
                .is_some();

            if !taken {
                break;
            }
            if attempt == 4 {
                error!("Could not generate a unique invite code after 5 attempts.");
                return Err(ApiError::AlreadyExists);
            }
            code = generate_invite_code();
        }

        let invite = BandInvite {
            id: Uuid::new_v4(),
            band_id,
            code,
            role,
            created_by,
            max_uses: payload.max_uses,
            uses_count: 0,
            expires_at,
            revoked_at: None,
            created_at: Utc::now().naive_utc(),
        };

        let mut tx = self.db.begin().await?;
        sqlx::query("SELECT id FROM bands WHERE id = $1 FOR UPDATE")
            .bind(band_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
        let creator_role: Option<BandRole> =
            sqlx::query_scalar("SELECT role FROM band_members WHERE band_id = $1 AND user_id = $2")
                .bind(band_id)
                .bind(created_by)
                .fetch_optional(&mut *tx)
                .await?;
        match creator_role {
            Some(creator) if creator.satisfies(BandRole::Admin) && role < creator => {}
            Some(_) => return Err(ApiError::Forbidden),
            None => return Err(ApiError::NotFound),
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM band_invites
             WHERE band_id = $1 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > $2)
               AND (max_uses IS NULL OR uses_count < max_uses)",
        )
        .bind(band_id)
        .bind(invite.created_at)
        .fetch_one(&mut *tx)
        .await?;
        if active >= MAX_ACTIVE_INVITES_PER_BAND {
            return Err(ApiError::quota_exceeded(
                "band_invites",
                MAX_ACTIVE_INVITES_PER_BAND,
            ));
        }

        sqlx::query(
            "INSERT INTO band_invites (id, band_id, code, role, created_by, max_uses, uses_count, expires_at, revoked_at, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(invite.id)
        .bind(invite.band_id)
        .bind(&invite.code)
        .bind(invite.role)
        .bind(invite.created_by)
        .bind(invite.max_uses)
        .bind(invite.uses_count)
        .bind(invite.expires_at)
        .bind(invite.revoked_at)
        .bind(invite.created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(invite)
    }

    async fn list(&self, band_id: Uuid) -> Result<Vec<BandInvite>, ApiError> {
        let invites = sqlx::query_as::<_, BandInvite>(
            "SELECT * FROM band_invites WHERE band_id = $1 ORDER BY created_at DESC;",
        )
        .bind(band_id)
        .fetch_all(&self.db)
        .await?;

        Ok(invites)
    }

    async fn find_by_code(&self, code: &str) -> Result<Option<BandInvite>, ApiError> {
        let invite = sqlx::query_as::<_, BandInvite>("SELECT * FROM band_invites WHERE code = $1;")
            .bind(code)
            .fetch_optional(&self.db)
            .await?;

        Ok(invite)
    }

    async fn revoke(&self, id: Uuid, band_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE band_invites SET revoked_at = $1 WHERE id = $2 AND band_id = $3 AND revoked_at IS NULL",
        )
        .bind(Utc::now().naive_utc())
        .bind(id)
        .bind(band_id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn redeem(
        &self,
        code: &str,
        user_id: Uuid,
        limits: InviteLimits,
    ) -> Result<(Uuid, BandRole), ApiError> {
        let invalid = || {
            ApiError::rule(
                axum::http::StatusCode::NOT_FOUND,
                crate::errors::api_error::codes::INVITE_INVALID,
                "This invite link is invalid, expired or has already been used.",
            )
        };

        let mut tx = self.db.begin().await?;

        let invite = sqlx::query_as::<_, BandInvite>(
            "SELECT * FROM band_invites WHERE code = $1 FOR UPDATE",
        )
        .bind(code.trim().to_uppercase())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(invalid)?;

        let now = Utc::now().naive_utc();
        let expired = invite.expires_at.is_some_and(|at| at <= now);
        let exhausted = invite.max_uses.is_some_and(|max| invite.uses_count >= max);
        if invite.revoked_at.is_some() || expired || exhausted {
            return Err(invalid());
        }

        sqlx::query("SELECT id FROM bands WHERE id = $1 FOR UPDATE")
            .bind(invite.band_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 11))")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let (band_members, memberships): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM band_members WHERE band_id = $1),
                    (SELECT COUNT(*) FROM band_members WHERE user_id = $2)",
        )
        .bind(invite.band_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(limit) = limits.band_members
            && band_members + 1 > limit
        {
            return Err(ApiError::quota_exceeded("band_members", limit));
        }
        if let Some(limit) = limits.band_memberships
            && memberships + 1 > limit
        {
            return Err(ApiError::quota_exceeded("band_memberships", limit));
        }

        let inserted = sqlx::query(
            "INSERT INTO band_members (id, band_id, user_id, role, joined_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (band_id, user_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(invite.band_id)
        .bind(user_id)
        .bind(invite.role)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        if inserted.rows_affected() == 0 {
            return Err(ApiError::rule(
                axum::http::StatusCode::CONFLICT,
                crate::errors::api_error::codes::ALREADY_MEMBER,
                "You are already a member of this band.",
            ));
        }

        sqlx::query("UPDATE band_invites SET uses_count = uses_count + 1 WHERE id = $1")
            .bind(invite.id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok((invite.band_id, invite.role))
    }

    async fn increment_uses(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("UPDATE band_invites SET uses_count = uses_count + 1 WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;

        Ok(())
    }
}
