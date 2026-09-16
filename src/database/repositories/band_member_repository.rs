use crate::{
    errors::api_error::ApiError,
    models::band::{BandMember, BandRole},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait BandMemberRepository: Send + Sync {
    /// Lists every member of a band, joined with their public user info.
    async fn list(&self, band_id: Uuid) -> Result<Vec<BandMember>, ApiError>;

    /// Adds a user to a band with the given role. Fails with `AlreadyExists`
    /// if they are already a member.
    async fn add_member(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError>;

    async fn update_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError>;

    /// Sets (or clears, with `None`) a member's free-text title/function
    /// label. Purely cosmetic — never checked for permissions.
    async fn update_title(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        title: Option<&str>,
    ) -> Result<(), ApiError>;

    async fn remove(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Counts how many members currently hold the `owner` role (always 0 or 1
    /// in practice, but never assumed so removal/demotion stays safe).
    async fn count_owners(&self, band_id: Uuid) -> Result<i64, ApiError>;

    /// Atomically swaps ownership: `new_owner_id` becomes `owner`,
    /// `current_owner_id` becomes `admin`. Fails if `new_owner_id` is not
    /// already a member of the band.
    async fn transfer_ownership(
        &self,
        band_id: Uuid,
        current_owner_id: Uuid,
        new_owner_id: Uuid,
    ) -> Result<(), ApiError>;
}

pub struct BandMemberRepositoryImpl {
    pub db: PgPool,
}

impl BandMemberRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BandMemberRepository for BandMemberRepositoryImpl {
    async fn list(&self, band_id: Uuid) -> Result<Vec<BandMember>, ApiError> {
        let members = sqlx::query_as::<_, BandMember>(
            r#"
            SELECT
                bm.id, bm.band_id, bm.user_id, bm.role, bm.title, bm.joined_at,
                u.username, u.first_name, u.last_name
            FROM band_members bm
            INNER JOIN users u ON u.id = bm.user_id
            WHERE bm.band_id = $1
            ORDER BY bm.role DESC, u.username ASC
            "#,
        )
        .bind(band_id)
        .fetch_all(&self.db)
        .await?;

        Ok(members)
    }

    async fn add_member(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError> {
        let already_member =
            sqlx::query("SELECT id FROM band_members WHERE band_id = $1 AND user_id = $2;")
                .bind(band_id)
                .bind(user_id)
                .fetch_optional(&self.db)
                .await?
                .is_some();

        if already_member {
            error!(%band_id, %user_id, "User is already a member of this band.");
            return Err(ApiError::AlreadyExists);
        }

        sqlx::query(
            "INSERT INTO band_members (id, band_id, user_id, role, joined_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(band_id)
        .bind(user_id)
        .bind(role)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn update_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError> {
        let result =
            sqlx::query("UPDATE band_members SET role = $1 WHERE band_id = $2 AND user_id = $3")
                .bind(role)
                .bind(band_id)
                .bind(user_id)
                .execute(&self.db)
                .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn update_title(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        title: Option<&str>,
    ) -> Result<(), ApiError> {
        let result =
            sqlx::query("UPDATE band_members SET title = $1 WHERE band_id = $2 AND user_id = $3")
                .bind(title)
                .bind(band_id)
                .bind(user_id)
                .execute(&self.db)
                .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn remove(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM band_members WHERE band_id = $1 AND user_id = $2")
            .bind(band_id)
            .bind(user_id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn count_owners(&self, band_id: Uuid) -> Result<i64, ApiError> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM band_members WHERE band_id = $1 AND role = 'owner';",
        )
        .bind(band_id)
        .fetch_one(&self.db)
        .await?;

        Ok(count)
    }

    async fn transfer_ownership(
        &self,
        band_id: Uuid,
        current_owner_id: Uuid,
        new_owner_id: Uuid,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        let new_owner_is_member =
            sqlx::query("SELECT id FROM band_members WHERE band_id = $1 AND user_id = $2;")
                .bind(band_id)
                .bind(new_owner_id)
                .fetch_optional(&mut *tx)
                .await?
                .is_some();

        if !new_owner_is_member {
            error!(%band_id, %new_owner_id, "Cannot transfer ownership to a non-member.");
            return Err(ApiError::NotFound);
        }

        sqlx::query("UPDATE band_members SET role = 'admin' WHERE band_id = $1 AND user_id = $2")
            .bind(band_id)
            .bind(current_owner_id)
            .execute(&mut *tx)
            .await?;

        sqlx::query("UPDATE band_members SET role = 'owner' WHERE band_id = $1 AND user_id = $2")
            .bind(band_id)
            .bind(new_owner_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(())
    }
}
