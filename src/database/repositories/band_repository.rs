use crate::database::repositories::quota_repository::QuotaGuard;
use crate::{
    errors::api_error::ApiError,
    models::band::{
        Band, BandPermission, BandRole, BandRolePermission, BandRolePermissionEntry,
        BandWithMembership, CreateBandPayload, UpdateBandPayload,
    },
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

    /// Creates a new band and makes `owner_id` its first member with the
    /// `owner` role, together with the band's repertoire (same transaction).
    /// Creates a band owned by `owner_id`, with its repertoire. `quota` is
    /// enforced inside the insert's transaction (see [`QuotaGuard`]).
    async fn create(
        &self,
        payload: &CreateBandPayload,
        owner_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Band, ApiError>;

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateBandPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Any band by ID, without a membership filter (staff tooling).
    async fn find_any(&self, id: Uuid) -> Result<Option<Band>, ApiError>;
    /// Marks a band as a favorite for this user. Idempotent.
    async fn add_favorite(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Un-favorites a band for this user. Idempotent.
    async fn remove_favorite(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Returns the caller's role in a band, or `None` if they are not a member.
    async fn role_of(&self, band_id: Uuid, user_id: Uuid) -> Result<Option<BandRole>, ApiError>;

    /// Ensures the band exists and the caller holds at least `min_role`.
    async fn require_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        min_role: BandRole,
    ) -> Result<BandRole, ApiError>;

    /// Returns the full `member`/`moderator` permission matrix for a band
    /// (six rows: one per role × permission combination).
    async fn get_role_permissions(
        &self,
        band_id: Uuid,
    ) -> Result<Vec<BandRolePermission>, ApiError>;

    /// Bulk-upserts a set of (role, permission, allowed) entries. Caller
    /// must already have checked the admin/owner role via `require_role`.
    async fn update_role_permissions(
        &self,
        band_id: Uuid,
        entries: &[BandRolePermissionEntry],
    ) -> Result<(), ApiError>;

    /// Returns `true` when `role` is allowed to perform `permission` in
    /// `band_id`. `admin` and `owner` are always `true`; for `member` and
    /// `moderator`, this reads the band's permission matrix, defaulting to
    /// `false` if no row exists (e.g. a race with band creation).
    async fn role_has_permission(
        &self,
        band_id: Uuid,
        role: BandRole,
        permission: BandPermission,
    ) -> Result<bool, ApiError>;

    /// The caller's role, when it grants `permission` in the band.
    /// `NotFound` for non-members, `Forbidden` without the permission.
    async fn require_permission(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        permission: BandPermission,
    ) -> Result<BandRole, ApiError>;

    /// The band's vote threshold for accepting suggestions automatically.
    async fn suggestion_threshold(&self, band_id: Uuid) -> Result<Option<i32>, ApiError>;
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
                b.id, b.name, b.slug, b.description, b.logo_url,
                b.created_by, b.updated_by,
                (SELECT u.username FROM users u WHERE u.id = b.updated_by) AS updated_by_username,
                b.created_at, b.updated_at,
                (SELECT COUNT(*) FROM band_members bm2 WHERE bm2.band_id = b.id) AS member_count,
                bm.role AS my_role,
                EXISTS(SELECT 1 FROM favorite_bands f WHERE f.band_id = b.id AND f.user_id = $1) AS is_favorite,
                (SELECT r.id FROM setlists r WHERE r.band_id = b.id AND r.is_repertoire) AS repertoire_id,
                b.suggestion_auto_accept_votes,
                (SELECT COUNT(*) FROM band_song_suggestions bs WHERE bs.band_id = b.id AND bs.status = 'open') AS open_suggestions,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'manage_setlists'), FALSE)) AS manage_setlists,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'manage_songs'), FALSE)) AS manage_songs,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'export_pdf'), FALSE)) AS export_pdf
            FROM bands b
            INNER JOIN band_members bm ON bm.band_id = b.id
            WHERE bm.user_id = $1
            ORDER BY is_favorite DESC, LOWER(b.name) ASC;
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
                b.id, b.name, b.slug, b.description, b.logo_url,
                b.created_by, b.updated_by,
                (SELECT u.username FROM users u WHERE u.id = b.updated_by) AS updated_by_username,
                b.created_at, b.updated_at,
                (SELECT COUNT(*) FROM band_members bm2 WHERE bm2.band_id = b.id) AS member_count,
                bm.role AS my_role,
                EXISTS(SELECT 1 FROM favorite_bands f WHERE f.band_id = b.id AND f.user_id = $2) AS is_favorite,
                (SELECT r.id FROM setlists r WHERE r.band_id = b.id AND r.is_repertoire) AS repertoire_id,
                b.suggestion_auto_accept_votes,
                (SELECT COUNT(*) FROM band_song_suggestions bs WHERE bs.band_id = b.id AND bs.status = 'open') AS open_suggestions,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'manage_setlists'), FALSE)) AS manage_setlists,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'manage_songs'), FALSE)) AS manage_songs,
                (bm.role IN ('admin', 'owner') OR COALESCE((SELECT p.allowed FROM band_role_permissions p
                    WHERE p.band_id = b.id AND p.role = bm.role AND p.permission = 'export_pdf'), FALSE)) AS export_pdf
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

    async fn create(
        &self,
        payload: &CreateBandPayload,
        owner_id: Uuid,
        quota: &[QuotaGuard],
    ) -> Result<Band, ApiError> {
        let name = payload.name.trim();
        let slug = self.generate_unique_slug(name).await?;
        let description = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        let new_band = Band::new(name, slug, description, owner_id);

        let mut tx = self.db.begin().await?;
        QuotaGuard::enforce_all(quota, &mut tx).await?;

        sqlx::query(
            "INSERT INTO bands (id, name, slug, description, logo_url, created_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(new_band.id)
        .bind(&new_band.name)
        .bind(&new_band.slug)
        .bind(&new_band.description)
        .bind(&new_band.logo_url)
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

        // Seed the default permission matrix for the two configurable
        // roles: moderators can manage setlists/songs, members can't
        // (until the admin opts them in), and everyone can export PDFs.
        // Every band has a repertoire from the start.
        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, band_id, is_repertoire, created_at, updated_at)
             VALUES ($1, 'Repertoire', NULL, $2, $3, TRUE, $4, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(owner_id)
        .bind(new_band.id)
        .bind(new_band.created_at)
        .execute(&mut *tx)
        .await?;

        for (role, permission, allowed) in [
            (BandRole::Moderator, BandPermission::ManageSetlists, true),
            (BandRole::Moderator, BandPermission::ManageSongs, true),
            (BandRole::Moderator, BandPermission::ExportPdf, true),
            (BandRole::Member, BandPermission::ManageSetlists, false),
            (BandRole::Member, BandPermission::ManageSongs, false),
            (BandRole::Member, BandPermission::ExportPdf, true),
        ] {
            sqlx::query(
                "INSERT INTO band_role_permissions (band_id, role, permission, allowed)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(new_band.id)
            .bind(role)
            .bind(permission)
            .bind(allowed)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        Ok(new_band)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateBandPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
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
            let description = description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty());
            sqlx::query("UPDATE bands SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(logo_url) = &payload.logo_url {
            let logo_url = logo_url.as_deref().map(str::trim).filter(|u| !u.is_empty());
            sqlx::query("UPDATE bands SET logo_url = $1 WHERE id = $2")
                .bind(logo_url)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(threshold) = payload.suggestion_auto_accept_votes {
            sqlx::query("UPDATE bands SET suggestion_auto_accept_votes = $1 WHERE id = $2")
                .bind(threshold)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE bands SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(id)
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Band>, ApiError> {
        let band = sqlx::query_as::<_, Band>(
            "SELECT id, name, slug, description, logo_url,
                    created_by, updated_by, created_at, updated_at
             FROM bands WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(band)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM bands WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn add_favorite(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO favorite_bands (user_id, band_id, created_at) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, band_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(band_id)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn remove_favorite(&self, band_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM favorite_bands WHERE user_id = $1 AND band_id = $2")
            .bind(user_id)
            .bind(band_id)
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

    async fn get_role_permissions(
        &self,
        band_id: Uuid,
    ) -> Result<Vec<BandRolePermission>, ApiError> {
        let permissions = sqlx::query_as::<_, BandRolePermission>(
            "SELECT role, permission, allowed FROM band_role_permissions
             WHERE band_id = $1
             ORDER BY role, permission;",
        )
        .bind(band_id)
        .fetch_all(&self.db)
        .await?;

        Ok(permissions)
    }

    async fn update_role_permissions(
        &self,
        band_id: Uuid,
        entries: &[BandRolePermissionEntry],
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        for entry in entries {
            sqlx::query(
                "INSERT INTO band_role_permissions (band_id, role, permission, allowed)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (band_id, role, permission) DO UPDATE SET allowed = $4",
            )
            .bind(band_id)
            .bind(entry.role)
            .bind(entry.permission)
            .bind(entry.allowed)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to update band role permission: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        sqlx::query("UPDATE bands SET updated_at = $1 WHERE id = $2")
            .bind(chrono::Utc::now().naive_utc())
            .bind(band_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(())
    }

    async fn role_has_permission(
        &self,
        band_id: Uuid,
        role: BandRole,
        permission: BandPermission,
    ) -> Result<bool, ApiError> {
        if role.satisfies(BandRole::Admin) {
            return Ok(true);
        }

        let allowed: Option<bool> = sqlx::query_scalar(
            "SELECT allowed FROM band_role_permissions
             WHERE band_id = $1 AND role = $2 AND permission = $3;",
        )
        .bind(band_id)
        .bind(role)
        .bind(permission)
        .fetch_optional(&self.db)
        .await?;

        Ok(allowed.unwrap_or(false))
    }

    async fn require_permission(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        permission: BandPermission,
    ) -> Result<BandRole, ApiError> {
        let role = self
            .role_of(band_id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if self.role_has_permission(band_id, role, permission).await? {
            Ok(role)
        } else {
            Err(ApiError::Forbidden)
        }
    }

    async fn suggestion_threshold(&self, band_id: Uuid) -> Result<Option<i32>, ApiError> {
        let threshold: Option<Option<i32>> =
            sqlx::query_scalar("SELECT suggestion_auto_accept_votes FROM bands WHERE id = $1")
                .bind(band_id)
                .fetch_optional(&self.db)
                .await?;
        Ok(threshold.flatten())
    }
}
