use crate::{
    errors::api_error::{ApiError, codes},
    models::{
        band::BandRole,
        gig::{CreateGigPayload, Gig, UpdateGigPayload},
    },
};
use axum::http::StatusCode;
use chrono::NaiveDateTime;
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`Gig`] from `gigs g`.
macro_rules! gig_columns {
    () => {
        "g.id, g.user_id, g.band_id,
         (SELECT sx.id FROM setlists sx WHERE sx.id = g.setlist_id AND sx.deleted_at IS NULL) AS setlist_id,
         g.venue, g.location, g.scheduled_at, g.status,
         g.notes, g.share_token, g.share_locked_at, g.share_lock_reason, g.updated_by,
         (SELECT u.username FROM users u WHERE u.id = g.updated_by) AS updated_by_username,
         (SELECT tx.id FROM tours tx WHERE tx.id = g.tour_id AND tx.deleted_at IS NULL) AS tour_id,
         (SELECT tx.name FROM tours tx WHERE tx.id = g.tour_id AND tx.deleted_at IS NULL) AS tour_name,
         g.created_at, g.updated_at"
    };
}

#[async_trait::async_trait]
pub trait GigRepository: Send + Sync {
    /// Lists the caller's personal gigs (i.e. `band_id IS NULL`).
    /// Optionally only the gigs of `tour_id`.
    async fn find_all(
        &self,
        user_id: Uuid,
        tour_id: Option<Uuid>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError>;
    /// Lists every live gig of a band (optionally of one tour). Callers
    /// must check membership first.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        tour_id: Option<Uuid>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError>;
    /// A gig the caller may view: their own personal gig, or any gig of a
    /// band they belong to.
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Gig>, ApiError>;
    /// Any gig by ID, without an access filter (staff tooling).
    async fn find_any(&self, id: Uuid) -> Result<Option<Gig>, ApiError>;
    async fn create(&self, payload: &CreateGigPayload, user_id: Uuid) -> Result<Gig, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateGigPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes a gig (trash purge).
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Moves a live gig to the trash.
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Personal owner, or a band member allowed to manage setlists (gigs
    /// follow the same permission).
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Generates a fresh share token (rotating any previous link). Fails
    /// with `SHARE_LOCKED` when staff disabled sharing for this gig.
    async fn enable_sharing(&self, id: Uuid) -> Result<Gig, ApiError>;
    async fn disable_sharing(&self, id: Uuid) -> Result<(), ApiError>;
    /// Public lookup — never resolves a locked share.
    async fn find_by_share_token(&self, token: &str) -> Result<Option<Gig>, ApiError>;
    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError>;
}

pub struct GigRepositoryImpl {
    pub db: PgPool,
}

impl GigRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

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
        tour_id: Option<Uuid>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gigs
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL
               AND ($2::uuid IS NULL OR tour_id = $2)",
        )
        .bind(user_id)
        .bind(tour_id)
        .fetch_one(&self.db);

        let gigs = sqlx::query_as::<_, Gig>(concat!(
            "SELECT ",
            gig_columns!(),
            " FROM gigs g
             WHERE g.user_id = $1 AND g.band_id IS NULL AND g.deleted_at IS NULL
               AND ($4::uuid IS NULL OR g.tour_id = $4)
             ORDER BY g.scheduled_at ASC, g.id ASC
             LIMIT $2 OFFSET $3"
        ))
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .bind(tour_id)
        .fetch_all(&self.db);

        let (count, gigs) = tokio::try_join!(count, gigs)?;
        Ok((gigs, count))
    }

    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        tour_id: Option<Uuid>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Gig>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gigs
             WHERE band_id = $1 AND deleted_at IS NULL AND ($2::uuid IS NULL OR tour_id = $2)",
        )
        .bind(band_id)
        .bind(tour_id)
        .fetch_one(&self.db);

        let gigs = sqlx::query_as::<_, Gig>(concat!(
            "SELECT ",
            gig_columns!(),
            " FROM gigs g
             WHERE g.band_id = $1 AND g.deleted_at IS NULL AND ($4::uuid IS NULL OR g.tour_id = $4)
             ORDER BY g.scheduled_at ASC, g.id ASC
             LIMIT $2 OFFSET $3"
        ))
        .bind(band_id)
        .bind(size)
        .bind(offset)
        .bind(tour_id)
        .fetch_all(&self.db);

        let (count, gigs) = tokio::try_join!(count, gigs)?;
        Ok((gigs, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Gig>, ApiError> {
        let gig = sqlx::query_as::<_, Gig>(concat!(
            "SELECT ",
            gig_columns!(),
            " FROM gigs g
             LEFT JOIN band_members bm ON bm.band_id = g.band_id AND bm.user_id = $2
             WHERE g.id = $1 AND g.deleted_at IS NULL
               AND ((g.band_id IS NULL AND g.user_id = $2) OR bm.user_id IS NOT NULL)"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(gig)
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Gig>, ApiError> {
        let gig = sqlx::query_as::<_, Gig>(concat!(
            "SELECT ",
            gig_columns!(),
            " FROM gigs g WHERE g.id = $1 AND g.deleted_at IS NULL"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(gig)
    }

    async fn create(&self, payload: &CreateGigPayload, user_id: Uuid) -> Result<Gig, ApiError> {
        let mut new_gig = Gig::new(payload.clone(), user_id);
        new_gig.venue = new_gig.venue.trim().to_string();
        new_gig.location = new_gig
            .location
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        new_gig.notes = new_gig
            .notes
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());

        sqlx::query(
            "INSERT INTO gigs (id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, tour_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
        .bind(new_gig.tour_id)
        .bind(new_gig.created_at)
        .bind(new_gig.updated_at)
        .execute(&self.db)
        .await?;

        Ok(new_gig)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateGigPayload,
        actor_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(venue) = &payload.venue {
            sqlx::query("UPDATE gigs SET venue = $1 WHERE id = $2")
                .bind(venue.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(location) = &payload.location {
            let location = location.as_deref().map(str::trim).filter(|l| !l.is_empty());
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

        if let Some(setlist_id) = payload.setlist_id {
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
            let notes = notes.as_deref().map(str::trim).filter(|n| !n.is_empty());
            sqlx::query("UPDATE gigs SET notes = $1 WHERE id = $2")
                .bind(notes)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(tour_id) = payload.tour_id {
            sqlx::query("UPDATE gigs SET tour_id = $1 WHERE id = $2")
                .bind(tour_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE gigs SET updated_at = $1, updated_by = $2 WHERE id = $3")
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
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

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE gigs SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(Uuid::new_v4())
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
            WHERE g.id = $1 AND g.deleted_at IS NULL
              AND ((g.band_id IS NULL AND g.user_id = $2) OR bm.user_id IS NOT NULL);
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .is_some();

        if exists {
            Ok(())
        } else {
            Err(ApiError::NotFound)
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
            WHERE g.id = $1 AND g.deleted_at IS NULL
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

        match (allowed, row.band_id, row.band_role) {
            (true, _, _) => Ok(()),
            (false, None, _) | (false, Some(_), None) => Err(ApiError::NotFound),
            (false, Some(_), Some(_)) => {
                error!(%id, %user_id, "User is not allowed to manage this gig.");
                Err(ApiError::Forbidden)
            }
        }
    }

    async fn enable_sharing(&self, id: Uuid) -> Result<Gig, ApiError> {
        let locked: Option<Option<NaiveDateTime>> = sqlx::query_scalar(
            "SELECT share_locked_at FROM gigs WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;

        match locked {
            None => return Err(ApiError::NotFound),
            Some(Some(_)) => {
                return Err(ApiError::rule(
                    StatusCode::FORBIDDEN,
                    codes::SHARE_LOCKED,
                    "Public sharing for this gig was disabled by a moderator.",
                ));
            }
            Some(None) => {}
        }

        let token = crate::utils::share_token::generate_share_token();

        sqlx::query("UPDATE gigs SET share_token = $1 WHERE id = $2 AND share_locked_at IS NULL")
            .bind(&token)
            .bind(id)
            .execute(&self.db)
            .await?;

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
        let gig = sqlx::query_as::<_, Gig>(concat!(
            "SELECT ",
            gig_columns!(),
            " FROM gigs g WHERE g.share_token = $1 AND g.share_locked_at IS NULL AND g.deleted_at IS NULL"
        ))
        .bind(token)
        .fetch_optional(&self.db)
        .await?;

        Ok(gig)
    }

    async fn lock_sharing(
        &self,
        id: Uuid,
        actor_id: Uuid,
        reason: Option<&str>,
    ) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE gigs
             SET share_token = NULL, share_locked_at = $1, share_locked_by = $2, share_lock_reason = $3
             WHERE id = $4",
        )
        .bind(chrono::Utc::now().naive_utc())
        .bind(actor_id)
        .bind(reason)
        .bind(id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn unlock_sharing(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE gigs SET share_locked_at = NULL, share_locked_by = NULL, share_lock_reason = NULL
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }
}
