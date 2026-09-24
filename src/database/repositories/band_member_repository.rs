use crate::{
    errors::api_error::ApiError,
    models::band::{BandMember, BandRole},
};
use sqlx::{PgConnection, PgPool};
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

    /// Sets a member's role (staff tooling). The owner's role is never
    /// changed here (`NotFound`), except that `Owner` may be given to a
    /// member of a band that has none (the single-owner index refuses a
    /// second one). A demoted member's pending invites are revoked.
    async fn update_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError>;

    /// `actor_id` changes `user_id`'s role to `role`, with the band's
    /// hierarchy checked under a lock on the band row (so a concurrent
    /// transfer, promotion or removal can't slip in between the check and
    /// the write): the actor must be `admin`+, the target must rank below
    /// them, and only the owner may grant a role as high as their own.
    /// `Owner` is never granted here. Returns the target's previous role.
    /// A demoted member's pending invites are revoked.
    async fn change_role(
        &self,
        band_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<BandRole, ApiError>;

    /// `actor_id` removes `user_id` from the band (or leaves it, when
    /// they're the same), with the hierarchy checked under the band row
    /// lock: removing someone else takes `admin`+ and a target ranking
    /// below the actor; the owner can never leave or be removed. The
    /// member's pending invites are revoked and their votes on open song
    /// suggestions withdrawn.
    async fn remove_as(&self, band_id: Uuid, actor_id: Uuid, user_id: Uuid)
    -> Result<(), ApiError>;

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
    ) -> Result<(), ApiError> {
        self.transfer_ownership_within(band_id, current_owner_id, new_owner_id, None)
            .await
    }

    /// [`transfer_ownership`](Self::transfer_ownership) under the band row
    /// lock, with conditional updates (`current_owner_id` must still be
    /// the owner: `CONFLICT` otherwise, so two racing transfers can't both
    /// win). With `owned_limit`, the recipient's `bands_owned` quota is
    /// checked in the same transaction (`QUOTA_EXCEEDED`).
    async fn transfer_ownership_within(
        &self,
        band_id: Uuid,
        current_owner_id: Uuid,
        new_owner_id: Uuid,
        owned_limit: Option<i64>,
    ) -> Result<(), ApiError>;
}

/// Locks the band row: every change to a band's memberships (transfers,
/// role changes, removals, invite redemptions) serializes on it.
async fn lock_band(tx: &mut PgConnection, band_id: Uuid) -> Result<(), ApiError> {
    sqlx::query("SELECT id FROM bands WHERE id = $1 FOR UPDATE")
        .bind(band_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(())
}

async fn role_in(
    tx: &mut PgConnection,
    band_id: Uuid,
    user_id: Uuid,
) -> Result<Option<BandRole>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT role FROM band_members WHERE band_id = $1 AND user_id = $2")
            .bind(band_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?,
    )
}

/// Revokes the invites `user_id` created for the band that can still be
/// used: they were minted with the authority of a role the member no
/// longer holds.
async fn revoke_invites_of(
    tx: &mut PgConnection,
    band_id: Uuid,
    user_id: Uuid,
) -> Result<u64, ApiError> {
    Ok(sqlx::query(
        "UPDATE band_invites SET revoked_at = $3
         WHERE band_id = $1 AND created_by = $2 AND revoked_at IS NULL",
    )
    .bind(band_id)
    .bind(user_id)
    .bind(chrono::Utc::now().naive_utc())
    .execute(&mut *tx)
    .await?
    .rows_affected())
}

/// Deletes the membership (never the owner's), revokes the member's
/// invites and withdraws their votes on open suggestions. `NotFound` when
/// there was nothing to delete.
async fn delete_membership(
    tx: &mut PgConnection,
    band_id: Uuid,
    user_id: Uuid,
) -> Result<(), ApiError> {
    let result = sqlx::query(
        "DELETE FROM band_members WHERE band_id = $1 AND user_id = $2 AND role <> 'owner'",
    )
    .bind(band_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    revoke_invites_of(tx, band_id, user_id).await?;
    // An ex-member's votes must not keep counting towards acceptance.
    sqlx::query(
        "DELETE FROM band_song_suggestion_votes v
         USING band_song_suggestions s
         WHERE v.suggestion_id = s.id AND s.band_id = $1 AND v.user_id = $2
           AND s.status = 'open'",
    )
    .bind(band_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn ownership_changed() -> ApiError {
    ApiError::rule(
        axum::http::StatusCode::CONFLICT,
        crate::errors::api_error::codes::INSUFFICIENT_ROLE,
        "Ownership of this band changed in the meantime. Reload and try again.",
    )
}

fn already_member() -> ApiError {
    ApiError::rule(
        axum::http::StatusCode::CONFLICT,
        crate::errors::api_error::codes::ALREADY_MEMBER,
        "This user is already a member of this band.",
    )
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
        // Atomic against concurrent joins via the (band_id, user_id)
        // unique constraint, rather than check-then-insert.
        let result = sqlx::query(
            "INSERT INTO band_members (id, band_id, user_id, role, joined_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (band_id, user_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(band_id)
        .bind(user_id)
        .bind(role)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            error!(%band_id, %user_id, "User is already a member of this band.");
            return Err(already_member());
        }

        Ok(())
    }

    async fn update_role(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        lock_band(&mut tx, band_id).await?;
        let previous = role_in(&mut tx, band_id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let result = sqlx::query(
            "UPDATE band_members SET role = $1
             WHERE band_id = $2 AND user_id = $3 AND (role <> 'owner' OR $1 = 'owner')",
        )
        .bind(role)
        .bind(band_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        if role < previous {
            revoke_invites_of(&mut tx, band_id, user_id).await?;
        }
        tx.commit().await?;

        Ok(())
    }

    async fn change_role(
        &self,
        band_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: BandRole,
    ) -> Result<BandRole, ApiError> {
        if role == BandRole::Owner || actor_id == user_id {
            return Err(ApiError::Forbidden);
        }
        let mut tx = self.db.begin().await?;
        lock_band(&mut tx, band_id).await?;
        let actor = role_in(&mut tx, band_id, actor_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if !actor.satisfies(BandRole::Admin) {
            return Err(ApiError::Forbidden);
        }
        let previous = role_in(&mut tx, band_id, user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        // Only members below the actor, and nobody but the owner hands out
        // a role as high as their own.
        if previous >= actor || (role >= actor && actor != BandRole::Owner) {
            error!(%band_id, %actor_id, %user_id, "Band role change outside the actor's authority.");
            return Err(ApiError::Forbidden);
        }
        // The same conditions again in the write itself: defense in depth
        // should the lock ever be bypassed.
        let result = sqlx::query(
            "UPDATE band_members SET role = $1
             WHERE band_id = $2 AND user_id = $3 AND role < $4 AND role <> 'owner'",
        )
        .bind(role)
        .bind(band_id)
        .bind(user_id)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::Forbidden);
        }
        if role < previous {
            revoke_invites_of(&mut tx, band_id, user_id).await?;
        }
        tx.commit().await?;
        Ok(previous)
    }

    async fn remove_as(
        &self,
        band_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        lock_band(&mut tx, band_id).await?;
        let actor = role_in(&mut tx, band_id, actor_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        if actor_id == user_id {
            // Leaving. The owner must hand the band over first.
            if actor == BandRole::Owner {
                error!(%band_id, %actor_id, "Band owner attempted to leave without transferring ownership first.");
                return Err(ApiError::Forbidden);
            }
        } else {
            let target = role_in(&mut tx, band_id, user_id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if !actor.satisfies(BandRole::Admin) || target >= actor {
                error!(%band_id, %actor_id, %user_id, "Cannot remove a member with an equal or higher role.");
                return Err(ApiError::Forbidden);
            }
        }
        delete_membership(&mut tx, band_id, user_id).await?;
        tx.commit().await?;
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
        let mut tx = self.db.begin().await?;
        lock_band(&mut tx, band_id).await?;
        delete_membership(&mut tx, band_id, user_id).await?;
        tx.commit().await?;
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

    async fn transfer_ownership_within(
        &self,
        band_id: Uuid,
        current_owner_id: Uuid,
        new_owner_id: Uuid,
        owned_limit: Option<i64>,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        lock_band(&mut tx, band_id).await?;

        match role_in(&mut tx, band_id, new_owner_id).await? {
            None => {
                error!(%band_id, %new_owner_id, "Cannot transfer ownership to a non-member.");
                return Err(ApiError::NotFound);
            }
            Some(BandRole::Owner) => return Err(ApiError::NotModified),
            Some(_) => {}
        }

        if let Some(limit) = owned_limit {
            let owned: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM band_members WHERE user_id = $1 AND role = 'owner'",
            )
            .bind(new_owner_id)
            .fetch_one(&mut *tx)
            .await?;
            if owned + 1 > limit {
                return Err(ApiError::quota_exceeded("bands_owned", limit));
            }
        }

        // Conditional: only the current owner is demoted, so of two racing
        // transfers the second finds no owner to demote and fails.
        let demoted = sqlx::query(
            "UPDATE band_members SET role = 'admin'
             WHERE band_id = $1 AND user_id = $2 AND role = 'owner'",
        )
        .bind(band_id)
        .bind(current_owner_id)
        .execute(&mut *tx)
        .await?;
        if demoted.rows_affected() != 1 {
            return Err(ownership_changed());
        }

        let promoted = sqlx::query(
            "UPDATE band_members SET role = 'owner'
             WHERE band_id = $1 AND user_id = $2 AND role <> 'owner'",
        )
        .bind(band_id)
        .bind(new_owner_id)
        .execute(&mut *tx)
        .await?;
        if promoted.rows_affected() != 1 {
            return Err(ownership_changed());
        }

        tx.commit().await?;

        Ok(())
    }
}
