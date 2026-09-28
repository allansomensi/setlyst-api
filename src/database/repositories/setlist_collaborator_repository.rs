use crate::{
    errors::api_error::{ApiError, codes},
    models::setlist_collaborator::{
        CollaboratorRole, MAX_SETLIST_COLLABORATORS, SetlistCollaborator, SetlistCollaborators,
        SetlistInvitation, SetlistOwner, SetlistStanding,
    },
};
use axum::http::StatusCode;
use sqlx::{PgPool, Postgres, Transaction};
use tracing::error;
use uuid::Uuid;

/// Collaborators of personal setlists (see `0017_setlist_collaboration.sql`).
///
/// Every change runs under a lock on the setlist row, with the actor's
/// standing read inside that transaction: a concurrent promotion, removal
/// or deletion can't slip in between the permission check and the write.
#[async_trait::async_trait]
pub trait SetlistCollaboratorRepository: Send + Sync {
    /// The caller's standing in a live personal setlist: its owner, an
    /// accepted collaborator, or `None` (a stranger, a pending invite, or a
    /// band setlist).
    async fn standing(
        &self,
        setlist_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SetlistStanding>, ApiError>;

    /// The owner and every collaborator, pending invites included.
    async fn list(&self, setlist_id: Uuid) -> Result<SetlistCollaborators, ApiError>;

    /// `actor_id` invites `user_id` as `role`. The actor must be allowed to
    /// manage that role (the owner, or a manager for viewers and editors).
    /// Refused for band setlists and the repertoire, for the owner, for
    /// someone already listed (`ALREADY_MEMBER`) and past
    /// [`MAX_SETLIST_COLLABORATORS`].
    async fn invite(
        &self,
        setlist_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: CollaboratorRole,
    ) -> Result<(), ApiError>;

    /// `actor_id` gives `user_id` (a collaborator or a pending invite) the
    /// role `role`. Both the target's current role and the new one must be
    /// manageable by the actor. Returns the previous role.
    async fn change_role(
        &self,
        setlist_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: CollaboratorRole,
    ) -> Result<CollaboratorRole, ApiError>;

    /// `actor_id` removes `user_id` (or withdraws their invite); a
    /// collaborator removing themselves leaves the setlist. The songs of
    /// their own library they had added leave the setlist with them: they
    /// are theirs, and the setlist can no longer show them.
    async fn remove(&self, setlist_id: Uuid, actor_id: Uuid, user_id: Uuid)
    -> Result<(), ApiError>;

    /// Accepts the caller's pending invite.
    async fn accept(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Declines (deletes) the caller's pending invite.
    async fn decline(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    /// Invites waiting for the caller's answer, newest first.
    async fn invitations(&self, user_id: Uuid) -> Result<Vec<SetlistInvitation>, ApiError>;
}

pub struct SetlistCollaboratorRepositoryImpl {
    pub db: PgPool,
}

impl SetlistCollaboratorRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn collaboration_unavailable() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::COLLABORATION_UNAVAILABLE,
        "Only personal setlists can have collaborators.",
    )
}

/// Locks the setlist row and returns the actor's standing in it.
/// `NotFound` for a setlist the actor can't see at all, and for band
/// setlists (`COLLABORATION_UNAVAILABLE` when the actor is in the band).
async fn lock_standing(
    tx: &mut Transaction<'_, Postgres>,
    setlist_id: Uuid,
    actor_id: Uuid,
) -> Result<SetlistStanding, ApiError> {
    let row: Option<(Uuid, Option<Uuid>, bool)> = sqlx::query_as(
        "SELECT user_id, band_id, is_repertoire FROM setlists
         WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(setlist_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((owner_id, band_id, is_repertoire)) = row else {
        return Err(ApiError::NotFound);
    };

    if band_id.is_some() || is_repertoire {
        let in_band: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM band_members WHERE band_id = $1 AND user_id = $2)",
        )
        .bind(band_id)
        .bind(actor_id)
        .fetch_one(&mut **tx)
        .await?;
        return Err(if in_band {
            collaboration_unavailable()
        } else {
            ApiError::NotFound
        });
    }

    if owner_id == actor_id {
        return Ok(SetlistStanding::Owner);
    }

    let role: Option<CollaboratorRole> = sqlx::query_scalar(
        "SELECT role FROM setlist_collaborators
         WHERE setlist_id = $1 AND user_id = $2 AND accepted_at IS NOT NULL",
    )
    .bind(setlist_id)
    .bind(actor_id)
    .fetch_optional(&mut **tx)
    .await?;
    role.map(SetlistStanding::Collaborator)
        .ok_or(ApiError::NotFound)
}

fn forbidden(setlist_id: Uuid, actor_id: Uuid, what: &str) -> ApiError {
    error!(%setlist_id, %actor_id, what, "Setlist collaborator change not allowed.");
    ApiError::Forbidden
}

#[async_trait::async_trait]
impl SetlistCollaboratorRepository for SetlistCollaboratorRepositoryImpl {
    async fn standing(
        &self,
        setlist_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SetlistStanding>, ApiError> {
        let row: Option<(Uuid, Option<CollaboratorRole>)> = sqlx::query_as(
            "SELECT s.user_id, c.role
             FROM setlists s
             LEFT JOIN setlist_collaborators c
                ON c.setlist_id = s.id AND c.user_id = $2 AND c.accepted_at IS NOT NULL
             WHERE s.id = $1 AND s.deleted_at IS NULL AND s.band_id IS NULL",
        )
        .bind(setlist_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;

        Ok(match row {
            Some((owner_id, _)) if owner_id == user_id => Some(SetlistStanding::Owner),
            Some((_, Some(role))) => Some(SetlistStanding::Collaborator(role)),
            _ => None,
        })
    }

    async fn list(&self, setlist_id: Uuid) -> Result<SetlistCollaborators, ApiError> {
        let owner = sqlx::query_as::<_, SetlistOwner>(
            "SELECT u.id AS user_id, u.username, u.first_name, u.last_name, u.avatar_url
             FROM setlists s INNER JOIN users u ON u.id = s.user_id
             WHERE s.id = $1 AND s.deleted_at IS NULL",
        )
        .bind(setlist_id)
        .fetch_optional(&self.db);

        let collaborators = sqlx::query_as::<_, SetlistCollaborator>(
            "SELECT c.user_id, u.username, u.first_name, u.last_name, u.avatar_url, c.role,
                    c.accepted_at IS NOT NULL AS accepted,
                    (SELECT x.username FROM users x WHERE x.id = c.invited_by) AS invited_by_username,
                    c.created_at, c.accepted_at
             FROM setlist_collaborators c
             INNER JOIN users u ON u.id = c.user_id
             WHERE c.setlist_id = $1
             ORDER BY c.accepted_at IS NULL, c.role DESC, LOWER(u.username), c.user_id",
        )
        .bind(setlist_id)
        .fetch_all(&self.db);

        let (owner, collaborators) = tokio::try_join!(owner, collaborators)?;
        Ok(SetlistCollaborators {
            owner: owner.ok_or(ApiError::NotFound)?,
            collaborators,
        })
    }

    async fn invite(
        &self,
        setlist_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: CollaboratorRole,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let standing = lock_standing(&mut tx, setlist_id, actor_id).await?;
        if !standing.can_manage(role) {
            return Err(forbidden(setlist_id, actor_id, "invite"));
        }

        let owner_id: Uuid = sqlx::query_scalar("SELECT user_id FROM setlists WHERE id = $1")
            .bind(setlist_id)
            .fetch_one(&mut *tx)
            .await?;
        if user_id == owner_id || user_id == actor_id {
            return Err(ApiError::rule(
                StatusCode::CONFLICT,
                codes::ALREADY_MEMBER,
                "This person already has access to the setlist.",
            ));
        }

        let (listed, count): (bool, i64) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM setlist_collaborators WHERE setlist_id = $1 AND user_id = $2),
                    (SELECT COUNT(*) FROM setlist_collaborators WHERE setlist_id = $1)",
        )
        .bind(setlist_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if listed {
            return Err(ApiError::rule(
                StatusCode::CONFLICT,
                codes::ALREADY_MEMBER,
                "This person was already invited to the setlist.",
            ));
        }
        if count >= MAX_SETLIST_COLLABORATORS {
            return Err(ApiError::quota_exceeded(
                "setlist_collaborators",
                MAX_SETLIST_COLLABORATORS,
            ));
        }

        sqlx::query(
            "INSERT INTO setlist_collaborators (setlist_id, user_id, role, invited_by, created_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(setlist_id)
        .bind(user_id)
        .bind(role)
        .bind(actor_id)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn change_role(
        &self,
        setlist_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
        role: CollaboratorRole,
    ) -> Result<CollaboratorRole, ApiError> {
        if actor_id == user_id {
            return Err(ApiError::rule(
                StatusCode::FORBIDDEN,
                codes::CANNOT_TARGET_SELF,
                "You can't change your own role.",
            ));
        }

        let mut tx = self.db.begin().await?;
        let standing = lock_standing(&mut tx, setlist_id, actor_id).await?;
        let current: CollaboratorRole = sqlx::query_scalar(
            "SELECT role FROM setlist_collaborators WHERE setlist_id = $1 AND user_id = $2",
        )
        .bind(setlist_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;

        if !standing.can_manage(current) || !standing.can_manage(role) {
            return Err(forbidden(setlist_id, actor_id, "change_role"));
        }
        if current == role {
            return Err(ApiError::NotModified);
        }

        sqlx::query(
            "UPDATE setlist_collaborators SET role = $3 WHERE setlist_id = $1 AND user_id = $2",
        )
        .bind(setlist_id)
        .bind(user_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(current)
    }

    async fn remove(
        &self,
        setlist_id: Uuid,
        actor_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        if actor_id != user_id {
            let standing = lock_standing(&mut tx, setlist_id, actor_id).await?;
            let current: CollaboratorRole = sqlx::query_scalar(
                "SELECT role FROM setlist_collaborators WHERE setlist_id = $1 AND user_id = $2",
            )
            .bind(setlist_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
            if !standing.can_manage(current) {
                return Err(forbidden(setlist_id, actor_id, "remove"));
            }
        } else {
            // Leaving: only needs the row lock (a pending invite is left
            // by declining it instead).
            sqlx::query("SELECT 1 FROM setlists WHERE id = $1 FOR UPDATE")
                .bind(setlist_id)
                .execute(&mut *tx)
                .await?;
        }

        let removed = sqlx::query(
            "DELETE FROM setlist_collaborators
             WHERE setlist_id = $1 AND user_id = $2
               AND ($3 OR accepted_at IS NOT NULL)",
        )
        .bind(setlist_id)
        .bind(user_id)
        .bind(actor_id != user_id)
        .execute(&mut *tx)
        .await?;
        if removed.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        sqlx::query(
            "DELETE FROM setlist_songs ss USING songs so
             WHERE ss.setlist_id = $1 AND so.id = ss.song_id
               AND so.user_id = $2 AND so.band_id IS NULL",
        )
        .bind(setlist_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query("UPDATE setlists SET updated_at = $2, updated_by = $3 WHERE id = $1")
            .bind(setlist_id)
            .bind(chrono::Utc::now().naive_utc())
            .bind(actor_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn accept(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE setlist_collaborators c SET accepted_at = $3
             FROM setlists s
             WHERE c.setlist_id = $1 AND c.user_id = $2 AND c.accepted_at IS NULL
               AND s.id = c.setlist_id AND s.deleted_at IS NULL",
        )
        .bind(setlist_id)
        .bind(user_id)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn decline(&self, setlist_id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "DELETE FROM setlist_collaborators
             WHERE setlist_id = $1 AND user_id = $2 AND accepted_at IS NULL",
        )
        .bind(setlist_id)
        .bind(user_id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn invitations(&self, user_id: Uuid) -> Result<Vec<SetlistInvitation>, ApiError> {
        Ok(sqlx::query_as::<_, SetlistInvitation>(
            "SELECT s.id AS setlist_id, s.title AS setlist_title,
                    o.id AS owner_id, o.username AS owner_username, o.avatar_url AS owner_avatar_url,
                    (SELECT x.username FROM users x WHERE x.id = c.invited_by) AS invited_by_username,
                    c.role, c.created_at
             FROM setlist_collaborators c
             INNER JOIN setlists s ON s.id = c.setlist_id
             INNER JOIN users o ON o.id = s.user_id
             WHERE c.user_id = $1 AND c.accepted_at IS NULL AND s.deleted_at IS NULL
             ORDER BY c.created_at DESC
             LIMIT 100",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?)
    }
}
