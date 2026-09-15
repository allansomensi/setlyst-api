use crate::{
    errors::api_error::ApiError,
    models::band::{Band, BandRole, BandWithMembership, CreateBandPayload, UpdateBandPayload},
    utils::slug::{slugify, uniquify_slug},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait BandRepository: Send + Sync {
    /// Lists every band the given user belongs to, along with their role in each.
    async fn find_all_for_user(&self, user_id: Uuid) -> Result<Vec<BandWithMembership>, ApiError>;

    /// Fetches a single band, scoped to a user who must be a member.
    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<BandWithMembership>, ApiError>;

    /// Creates a new band and makes `owner_id` its first member with the `owner` role.
    async fn create(&self, payload: &CreateBandPayload, owner_id: Uuid) -> Result<Band, ApiError>;

    async fn update(&self, id: Uuid, payload: &UpdateBandPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;

    /// Returns the caller's role in a band, or `None` if they are not a member.
    async fn role_of(&self, band_id: Uuid, user_id: Uuid) -> Result<Option<BandRole>, ApiError>;

    /// Ensures the band exists and the caller holds at least `min_role`.
    async fn require_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        min_role: BandRole,
    ) -> Result<BandRole, ApiError>;
}

pub struct BandRepositoryImpl {
    pub db: PgPool,
}

impl BandRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Finds a slug that isn't taken yet, starting from `slugify(name)` and
    /// falling back to a randomized suffix on collision.
    async fn generate_unique_slug(&self, name: &str) -> Result<String, ApiError> {
        let base = slugify(name);

        for candidate in std::iter::once(base.clone())
            .chain(std::iter::repeat_with(|| uniquify_slug(&base)).take(5))
        {
            let taken = sqlx::query("SELECT id FROM bands WHERE slug = $1;")
                .bind(&candidate)
                .fetch_optional(&self.db)
                .await?
                .is_some();

            if !taken {
                return Ok(candidate);
            }
        }

        Err(ApiError::AlreadyExists)
    }
}

// sqlx 0.9 requires query strings to be `&'static str` literals (it added
// this check to make SQL auditable and rule out runtime-built queries), so
// the two queries below are written out in full rather than assembled with
// `format!` from a shared prefix. Keep them in sync if the shape changes.

#[async_trait::async_trait]
impl BandRepository for BandRepositoryImpl {
    async fn find_all_for_user(&self, user_id: Uuid) -> Result<Vec<BandWithMembership>, ApiError> {
        let bands = sqlx::query_as::<_, BandWithMembership>(
            r#"
            SELECT
                b.id, b.name, b.slug, b.description, b.logo_url, b.members_can_manage_setlists,
                b.created_by, b.created_at, b.updated_at,
                (SELECT COUNT(*) FROM band_members bm2 WHERE bm2.band_id = b.id) AS member_count,
                bm.role AS my_role
            FROM bands b
            INNER JOIN band_members bm ON bm.band_id = b.id
            WHERE bm.user_id = $1
            ORDER BY b.name ASC;
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        Ok(bands)
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<BandWithMembership>, ApiError> {
        let band = sqlx::query_as::<_, BandWithMembership>(
            r#"
            SELECT
                b.id, b.name, b.slug, b.description, b.logo_url, b.members_can_manage_setlists,
                b.created_by, b.created_at, b.updated_at,
                (SELECT COUNT(*) FROM band_members bm2 WHERE bm2.band_id = b.id) AS member_count,
                bm.role AS my_role
            FROM bands b
            INNER JOIN band_members bm ON bm.band_id = b.id
            WHERE b.id = $1 AND bm.user_id = $2;
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        Ok(band)
    }

    async fn create(&self, payload: &CreateBandPayload, owner_id: Uuid) -> Result<Band, ApiError> {
        let name = payload.name.trim();
        let slug = self.generate_unique_slug(name).await?;
        let new_band = Band::new(name, slug, payload.description.clone(), owner_id);

        let mut tx = self.db.begin().await?;

        sqlx::query(
            "INSERT INTO bands (id, name, slug, description, logo_url, members_can_manage_setlists, created_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(new_band.id)
        .bind(&new_band.name)
        .bind(&new_band.slug)
        .bind(&new_band.description)
        .bind(&new_band.logo_url)
        .bind(new_band.members_can_manage_setlists)
        .bind(new_band.created_by)
        .bind(new_band.created_at)
        .bind(new_band.updated_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO band_members (id, band_id, user_id, role, joined_at) VALUES ($1, $2, $3, 'owner', $4)",
        )
        .bind(Uuid::new_v4())
        .bind(new_band.id)
        .bind(owner_id)
        .bind(new_band.created_at)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(new_band)
    }

    async fn update(&self, id: Uuid, payload: &UpdateBandPayload) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(name) = &payload.name {
            sqlx::query("UPDATE bands SET name = $1 WHERE id = $2")
                .bind(name.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE bands SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(logo_url) = &payload.logo_url {
            sqlx::query("UPDATE bands SET logo_url = $1 WHERE id = $2")
                .bind(logo_url)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(members_can_manage_setlists) = payload.members_can_manage_setlists {
            sqlx::query("UPDATE bands SET members_can_manage_setlists = $1 WHERE id = $2")
                .bind(members_can_manage_setlists)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE bands SET updated_at = $1 WHERE id = $2")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(id)
        } else {
            Err(ApiError::NotModified)
        }
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM bands WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn role_of(&self, band_id: Uuid, user_id: Uuid) -> Result<Option<BandRole>, ApiError> {
        let role = sqlx::query_scalar::<_, BandRole>(
            "SELECT role FROM band_members WHERE band_id = $1 AND user_id = $2;",
        )
        .bind(band_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        Ok(role)
    }

    async fn require_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        min_role: BandRole,
    ) -> Result<BandRole, ApiError> {
        match self.role_of(band_id, user_id).await? {
            Some(role) if role.satisfies(min_role) => Ok(role),
            Some(_) => {
                error!(%band_id, %user_id, "Band member does not have the required role.");
                Err(ApiError::Forbidden)
            }
            None => {
                error!(%band_id, %user_id, "User is not a member of this band.");
                Err(ApiError::NotFound)
            }
        }
    }
}
