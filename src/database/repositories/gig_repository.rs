use crate::{
    errors::api_error::ApiError,
    models::{
        band::BandRole,
        gig::{CreateGigPayload, Gig, UpdateGigPayload},
    },
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait GigRepository: Send + Sync {
    /// Lists the caller's personal gigs (i.e. `band_id IS NULL`), soonest
    /// first.
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError>;
    /// Lists every gig that belongs to a band. Callers must check band
    /// membership themselves before calling this.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError>;
    /// Fetches a gig the caller may *view*: either their own personal gig,
    /// or a gig belonging to any band they are a member of.
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Gig>, ApiError>;
    async fn create(&self, payload: &CreateGigPayload, user_id: Uuid) -> Result<Gig, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateGigPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may *view* the gig (see `find_by_id`).
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Checks the caller may *manage* (edit/delete) the gig: its personal
    /// owner, or a band member whose role clears the band's
    /// setlist-management bar (`moderator`+, or `member` when the band
    /// allows it) — the same bar used for the band's setlists.
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Generates a fresh public share token for the gig, replacing any
    /// existing one (which immediately invalidates previously shared
    /// links). Returns the updated gig.
    async fn enable_sharing(&self, id: Uuid) -> Result<Gig, ApiError>;
    /// Disables public sharing (clears the share token).
    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Resolves a gig by its public share token. No ownership or
    /// membership filter — this is the lookup used by the unauthenticated
    /// `/public/gigs/{token}` routes.
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Gig>, ApiError>;
}

pub struct GigRepositoryImpl {
    pub db: PgPool,
}

impl GigRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// Row shape used to decide write permission on a gig without fetching its
/// full column set. Mirrors `setlist_repository::SetlistAccessRow`.
#[derive(sqlx::FromRow)]
struct GigAccessRow {
    owner_id: Uuid,
    band_id: Option<Uuid>,
    band_role: Option<BandRole>,
    role_permission_allowed: Option<bool>,
}

#[async_trait::async_trait]
impl GigRepository for GigRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count =
            sqlx::query_scalar("SELECT COUNT(*) FROM gigs WHERE user_id = $1 AND band_id IS NULL;")
                .bind(user_id)
                .fetch_one(&self.db);

        let gigs = sqlx::query_as::<_, Gig>(
            "SELECT id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, share_token, created_at, updated_at
             FROM gigs
             WHERE user_id = $1 AND band_id IS NULL
             ORDER BY scheduled_at ASC
             LIMIT $2 OFFSET $3"
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, gigs) = tokio::try_join!(count, gigs)?;
        Ok((gigs, count))
    }

    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM gigs WHERE band_id = $1;")
            .bind(band_id)
            .fetch_one(&self.db);

        let gigs = sqlx::query_as::<_, Gig>(
            "SELECT id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, share_token, created_at, updated_at
             FROM gigs
             WHERE band_id = $1
             ORDER BY scheduled_at ASC
             LIMIT $2 OFFSET $3"
        )
        .bind(band_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, gigs) = tokio::try_join!(count, gigs)?;
        Ok((gigs, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Gig>, ApiError> {
        let gig = sqlx::query_as::<_, Gig>(
            "SELECT g.id, g.user_id, g.band_id, g.setlist_id, g.venue, g.location, g.scheduled_at, g.status, g.notes, g.share_token, g.created_at, g.updated_at
             FROM gigs g
             LEFT JOIN band_members bm ON bm.band_id = g.band_id AND bm.user_id = $2
             WHERE g.id = $1 AND (g.user_id = $2 OR bm.user_id IS NOT NULL)"
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(gig)
    }

    async fn create(&self, payload: &CreateGigPayload, user_id: Uuid) -> Result<Gig, ApiError> {
        let new_gig = Gig::new(payload.clone(), user_id);

        sqlx::query(
            "INSERT INTO gigs (id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(new_gig.id)
        .bind(new_gig.user_id)
        .bind(new_gig.band_id)
        .bind(new_gig.setlist_id)
        .bind(&new_gig.venue)
        .bind(&new_gig.location)
        .bind(new_gig.scheduled_at)
        .bind(new_gig.status)
        .bind(&new_gig.notes)
        .bind(new_gig.created_at)
        .bind(new_gig.updated_at)
        .execute(&self.db)
        .await?;

        Ok(new_gig)
    }

    async fn update(&self, id: Uuid, payload: &UpdateGigPayload) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(venue) = &payload.venue {
            sqlx::query("UPDATE gigs SET venue = $1 WHERE id = $2")
                .bind(venue)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(location) = &payload.location {
            sqlx::query("UPDATE gigs SET location = $1 WHERE id = $2")
                .bind(location)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(scheduled_at) = payload.scheduled_at {
            sqlx::query("UPDATE gigs SET scheduled_at = $1 WHERE id = $2")
                .bind(scheduled_at)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(setlist_id) = &payload.setlist_id {
            sqlx::query("UPDATE gigs SET setlist_id = $1 WHERE id = $2")
                .bind(setlist_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(status) = payload.status {
            sqlx::query("UPDATE gigs SET status = $1 WHERE id = $2")
                .bind(status)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(notes) = &payload.notes {
            sqlx::query("UPDATE gigs SET notes = $1 WHERE id = $2")
                .bind(notes)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE gigs SET updated_at = $1 WHERE id = $2")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;

        if !updated {
            return Err(ApiError::NotModified);
        }

        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM gigs WHERE id = $1;")
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query(
            r#"
            SELECT g.id FROM gigs g
            LEFT JOIN band_members bm ON bm.band_id = g.band_id AND bm.user_id = $2
            WHERE g.id = $1 AND (g.user_id = $2 OR bm.user_id IS NOT NULL);
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if !exists {
            error!("Gig ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let row = sqlx::query_as::<_, GigAccessRow>(
            r#"
            SELECT
                g.user_id AS owner_id,
                g.band_id,
                bm.role AS band_role,
                brp.allowed AS role_permission_allowed
            FROM gigs g
            LEFT JOIN band_members bm ON bm.band_id = g.band_id AND bm.user_id = $2
            LEFT JOIN band_role_permissions brp
                ON brp.band_id = g.band_id
                AND brp.role = bm.role
                AND brp.permission = 'manage_setlists'
            WHERE g.id = $1
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        let allowed = match row.band_id {
            None => row.owner_id == user_id,
            Some(_) => match row.band_role {
                Some(role) if role.satisfies(BandRole::Admin) => true,
                Some(_) => row.role_permission_allowed.unwrap_or(false),
                None => false,
            },
        };

        if allowed {
            Ok(())
        } else {
            error!(%id, %user_id, "User is not allowed to manage this gig.");
            Err(ApiError::Forbidden)
        }
    }

    async fn enable_sharing(&self, id: Uuid) -> Result<Gig, ApiError> {
        let token = crate::utils::share_token::generate_share_token();

        let result = sqlx::query("UPDATE gigs SET share_token = $1 WHERE id = $2")
            .bind(&token)
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        self.find_by_share_token(&token)
            .await?
            .ok_or(ApiError::NotFound)
    }

    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("UPDATE gigs SET share_token = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn find_by_share_token(&self, token: &str) -> Result<Option<Gig>, ApiError> {
        let gig = sqlx::query_as::<_, Gig>(
            "SELECT id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, share_token, created_at, updated_at
             FROM gigs WHERE share_token = $1"
        )
        .bind(token)
        .fetch_optional(&self.db)
        .await?;

        Ok(gig)
    }
}
