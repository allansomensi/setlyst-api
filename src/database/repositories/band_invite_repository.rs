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
        let expires_at: Option<NaiveDateTime> = payload
            .expires_in_hours
            .map(|hours| Utc::now().naive_utc() + chrono::Duration::hours(hours));

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
        .execute(&self.db)
        .await?;

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

    async fn increment_uses(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("UPDATE band_invites SET uses_count = uses_count + 1 WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;

        Ok(())
    }
}
