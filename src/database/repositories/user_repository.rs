use crate::{
    errors::api_error::ApiError,
    models::user::{
        CreateUserPayload, Role, Status, UpdateUserPayload, User, UserPublic, UsernameHistoryEntry,
        clearable,
    },
    utils::hashing::encrypt_password,
};
use chrono::NaiveDateTime;
use sqlx::{FromRow, PgPool};
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`UserPublic`] from `users u`. A macro (not a
/// `const`) so it can be spliced into `concat!` — sqlx only accepts
/// `&'static str` literals as query strings.
macro_rules! user_public_columns {
    () => {
        "u.id, u.username, u.email, u.first_name, u.last_name, u.role, u.status,
         u.username_changed_at, u.must_change_password, u.password_changed_at, u.last_login_at,
         (u.banned_at IS NOT NULL AND (u.banned_until IS NULL OR u.banned_until > (NOW() AT TIME ZONE 'utc'))) AS is_banned,
         u.banned_at, u.banned_until, u.ban_reason,
         (SELECT x.username FROM users x WHERE x.id = u.banned_by) AS banned_by_username,
         (SELECT x.username FROM users x WHERE x.id = u.created_by) AS created_by_username,
         (SELECT x.username FROM users x WHERE x.id = u.updated_by) AS updated_by_username,
         u.created_at, u.updated_at"
    };
}

/// Columns selected for a full [`User`] (including secrets) from `users`.
macro_rules! user_account_columns {
    () => {
        "id, username, email, password_hash, first_name, last_name, role, status,
         token_version, must_change_password, banned_at, banned_until, ban_reason,
         username_changed_at, created_at, updated_at"
    };
}

/// The per-request authorization snapshot the authentication middleware
/// checks every token against.
#[derive(Debug, Clone, FromRow)]
pub struct AuthState {
    pub username: String,
    pub role: Role,
    pub status: Status,
    pub token_version: i32,
    pub must_change_password: bool,
    pub banned_at: Option<NaiveDateTime>,
    pub banned_until: Option<NaiveDateTime>,
    pub ban_reason: Option<String>,
}

/// Optional case-insensitive search over the account's names; `$n` is a
/// nullable, already-escaped `ILIKE` pattern.
macro_rules! user_search_filter {
    ($param:literal) => {
        concat!(
            "(",
            $param,
            "::text IS NULL OR u.username ILIKE ",
            $param,
            " OR u.email ILIKE ",
            $param,
            " OR u.first_name ILIKE ",
            $param,
            " OR u.last_name ILIKE ",
            $param,
            ")"
        )
    };
}

#[async_trait::async_trait]
pub trait UserRepository: Send + Sync {
    /// `search` is an escaped `ILIKE` pattern matched against username,
    /// email and names.
    async fn find_all(
        &self,
        page: i64,
        size: i64,
        search: Option<&str>,
    ) -> Result<(Vec<UserPublic>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserPublic>, ApiError>;
    /// Full account row (with secrets) by username, case-insensitively.
    async fn find_by_username(&self, username: &str) -> Result<Option<User>, ApiError>;
    /// Full account row (with secrets) by ID.
    async fn find_account(&self, id: Uuid) -> Result<Option<User>, ApiError>;
    async fn auth_state(&self, id: Uuid) -> Result<Option<AuthState>, ApiError>;
    /// Creates an account. `created_by` is the staff member creating it
    /// (`None` for self-registration); `must_change_password` forces a new
    /// password at first sign-in.
    async fn create(
        &self,
        payload: &CreateUserPayload,
        created_by: Option<Uuid>,
        must_change_password: bool,
    ) -> Result<User, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateUserPayload,
        actor_id: Option<Uuid>,
    ) -> Result<Uuid, ApiError>;
    /// Permanently deletes an account without orphaning band data: bands
    /// the user owns are handed to their next most senior member (or
    /// deleted if nobody else is left), and band-owned rows the user
    /// happened to create are re-attributed to the band's owner instead of
    /// being cascade-deleted with the account.
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(&self, username: &str, exclude_id: Option<Uuid>) -> Result<(), ApiError>;
    /// `true` if `username` is free to take (case-insensitively).
    async fn is_username_available(
        &self,
        username: &str,
        exclude_id: Option<Uuid>,
    ) -> Result<bool, ApiError>;
    async fn exists(&self, user_id: Uuid) -> Result<(), ApiError>;
    /// Records a successful login and returns whether it was the first.
    async fn mark_login(&self, user_id: Uuid) -> Result<bool, ApiError>;
    async fn get_username_history(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<UsernameHistoryEntry>, ApiError>;
    /// Replaces the password hash, sets the must-change flag and — when
    /// `revoke_sessions` — invalidates every token issued so far.
    async fn set_password(
        &self,
        id: Uuid,
        new_password: &str,
        must_change_password: bool,
        revoke_sessions: bool,
        actor_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    async fn set_must_change_password(&self, id: Uuid, value: bool) -> Result<(), ApiError>;
    /// Invalidates every token issued so far for this account.
    async fn revoke_sessions(&self, id: Uuid) -> Result<(), ApiError>;
    async fn ban(
        &self,
        id: Uuid,
        until: Option<NaiveDateTime>,
        reason: Option<&str>,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;
    async fn unban(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Admins who could still sign in (active, not suspended).
    async fn count_active_admins(&self) -> Result<i64, ApiError>;
}

pub struct UserRepositoryImpl {
    pub db: PgPool,
}

impl UserRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn now() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

#[async_trait::async_trait]
impl UserRepository for UserRepositoryImpl {
    async fn find_all(
        &self,
        page: i64,
        size: i64,
        search: Option<&str>,
    ) -> Result<(Vec<UserPublic>, i64), ApiError> {
        let offset = (page - 1) * size;
        let count = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM users u WHERE ",
            user_search_filter!("$1")
        ))
        .bind(search)
        .fetch_one(&self.db);
        let users = sqlx::query_as::<_, UserPublic>(concat!(
            "SELECT ",
            user_public_columns!(),
            " FROM users u WHERE ",
            user_search_filter!("$3"),
            " ORDER BY LOWER(u.username) ASC LIMIT $1 OFFSET $2"
        ))
        .bind(size)
        .bind(offset)
        .bind(search)
        .fetch_all(&self.db);

        let (count, users) = tokio::try_join!(count, users)?;
        Ok((users, count))
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserPublic>, ApiError> {
        let user = sqlx::query_as::<_, UserPublic>(concat!(
            "SELECT ",
            user_public_columns!(),
            " FROM users u WHERE u.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(user)
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<User>, ApiError> {
        let user = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_account_columns!(),
            " FROM users WHERE LOWER(username) = LOWER($1)"
        ))
        .bind(username.trim())
        .fetch_optional(&self.db)
        .await?;

        Ok(user)
    }

    async fn find_account(&self, id: Uuid) -> Result<Option<User>, ApiError> {
        let user = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_account_columns!(),
            " FROM users WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;

        Ok(user)
    }

    async fn auth_state(&self, id: Uuid) -> Result<Option<AuthState>, ApiError> {
        let state = sqlx::query_as::<_, AuthState>(
            "SELECT username, role, status, token_version, must_change_password,
                    banned_at, banned_until, ban_reason
             FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;

        Ok(state)
    }

    async fn create(
        &self,
        payload: &CreateUserPayload,
        created_by: Option<Uuid>,
        must_change_password: bool,
    ) -> Result<User, ApiError> {
        let email = clearable(&payload.email).flatten();
        let first_name = clearable(&payload.first_name).flatten();
        let last_name = clearable(&payload.last_name).flatten();

        let mut new_user = User::new(
            payload.username.trim(),
            email,
            encrypt_password(&payload.password)?.as_str(),
            first_name,
            last_name,
            payload.role.clone(),
            payload.status.clone(),
        );
        new_user.must_change_password = must_change_password;

        sqlx::query(
            r#"INSERT INTO users (id, username, email, password_hash, first_name, last_name, role, status,
                                  must_change_password, password_changed_at, created_by, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        )
        .bind(new_user.id)
        .bind(&new_user.username)
        .bind(&new_user.email)
        .bind(&new_user.password_hash)
        .bind(&new_user.first_name)
        .bind(&new_user.last_name)
        .bind(&new_user.role)
        .bind(&new_user.status)
        .bind(new_user.must_change_password)
        .bind(new_user.created_at)
        .bind(created_by)
        .bind(new_user.created_at)
        .bind(new_user.updated_at)
        .execute(&self.db)
        .await?;

        Ok(new_user)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateUserPayload,
        actor_id: Option<Uuid>,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(username) = &payload.username {
            let username = username.trim();
            // Capture the pre-update username via the `old` subquery in
            // FROM (evaluated against the row as it stood before this
            // statement's SET applies) so the history row and the change
            // itself land in one atomic round trip. Skip recording history
            // (and bumping the cooldown) when the new value is byte-for-byte
            // identical to the old one — not a real change.
            let previous_username: Option<String> = sqlx::query_scalar(
                r#"
                UPDATE users u
                SET username = $2, username_changed_at = $3
                FROM (SELECT username FROM users WHERE id = $1) AS old
                WHERE u.id = $1 AND old.username IS DISTINCT FROM $2
                RETURNING old.username
                "#,
            )
            .bind(id)
            .bind(username)
            .bind(now())
            .fetch_optional(&mut *tx)
            .await?;

            if let Some(previous_username) = previous_username {
                sqlx::query(
                    "INSERT INTO username_history (id, user_id, old_username, changed_at) VALUES ($1, $2, $3, $4)",
                )
                .bind(Uuid::new_v4())
                .bind(id)
                .bind(&previous_username)
                .bind(now())
                .execute(&mut *tx)
                .await?;
                updated = true;
            }
        }

        if let Some(email) = clearable(&payload.email) {
            sqlx::query("UPDATE users SET email = $1 WHERE id = $2;")
                .bind(email)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(first_name) = clearable(&payload.first_name) {
            sqlx::query("UPDATE users SET first_name = $1 WHERE id = $2;")
                .bind(first_name)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(last_name) = clearable(&payload.last_name) {
            sqlx::query("UPDATE users SET last_name = $1 WHERE id = $2;")
                .bind(last_name)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(role) = &payload.role {
            sqlx::query("UPDATE users SET role = $1 WHERE id = $2;")
                .bind(role)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(status) = &payload.status {
            sqlx::query("UPDATE users SET status = $1 WHERE id = $2;")
                .bind(status)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if !updated {
            return Err(ApiError::NotModified);
        }

        sqlx::query("UPDATE users SET updated_at = $1, updated_by = $2 WHERE id = $3;")
            .bind(now())
            .bind(actor_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        // 1. Hand every band this user owns to the most senior remaining
        //    member (highest role, then longest-standing). Bands with no
        //    other member are deleted — nobody is left to own them.
        let owned_bands: Vec<Uuid> = sqlx::query_scalar(
            "SELECT band_id FROM band_members WHERE user_id = $1 AND role = 'owner'",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;

        for band_id in owned_bands {
            let successor: Option<Uuid> = sqlx::query_scalar(
                "SELECT user_id FROM band_members
                 WHERE band_id = $1 AND user_id != $2
                 ORDER BY role DESC, joined_at ASC
                 LIMIT 1",
            )
            .bind(band_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

            match successor {
                Some(successor) => {
                    sqlx::query(
                        "UPDATE band_members SET role = 'owner' WHERE band_id = $1 AND user_id = $2",
                    )
                    .bind(band_id)
                    .bind(successor)
                    .execute(&mut *tx)
                    .await?;
                }
                None => {
                    sqlx::query("DELETE FROM bands WHERE id = $1")
                        .bind(band_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }

        // 2. Band-owned rows record their creator in `user_id` purely for
        //    audit, but the column cascades on delete — which would take a
        //    band's shared songs, setlists and gigs down with one member's
        //    account. Re-attribute them to the band's (possibly new) owner.
        sqlx::query(
            "UPDATE setlists s SET user_id = bm.user_id
             FROM band_members bm
             WHERE s.user_id = $1 AND s.band_id IS NOT NULL
               AND bm.band_id = s.band_id AND bm.role = 'owner' AND bm.user_id != $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE gigs g SET user_id = bm.user_id
             FROM band_members bm
             WHERE g.user_id = $1 AND g.band_id IS NOT NULL
               AND bm.band_id = g.band_id AND bm.role = 'owner' AND bm.user_id != $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE songs s SET user_id = bm.user_id
             FROM band_members bm
             WHERE s.user_id = $1 AND s.band_id IS NOT NULL
               AND bm.band_id = s.band_id AND bm.role = 'owner' AND bm.user_id != $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE artists a SET user_id = bm.user_id
             FROM band_members bm
             WHERE a.user_id = $1 AND a.band_id IS NOT NULL
               AND bm.band_id = a.band_id AND bm.role = 'owner' AND bm.user_id != $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;

        let result = sqlx::query("DELETE FROM users WHERE id = $1;")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        tx.commit().await?;
        Ok(())
    }

    async fn is_unique(&self, username: &str, exclude_id: Option<Uuid>) -> Result<(), ApiError> {
        // Case-insensitive: "Augusto" and "augusto" count as the same name.
        let exists = match exclude_id {
            Some(id) => {
                sqlx::query("SELECT id FROM users WHERE LOWER(username) = LOWER($1) AND id != $2;")
                    .bind(username.trim())
                    .bind(id)
                    .fetch_optional(&self.db)
                    .await?
                    .is_some()
            }
            None => sqlx::query("SELECT id FROM users WHERE LOWER(username) = LOWER($1);")
                .bind(username.trim())
                .fetch_optional(&self.db)
                .await?
                .is_some(),
        };

        if exists {
            Err(ApiError::rule(
                axum::http::StatusCode::CONFLICT,
                crate::errors::api_error::codes::USERNAME_TAKEN,
                "This username is already taken.",
            ))
        } else {
            Ok(())
        }
    }

    async fn is_username_available(
        &self,
        username: &str,
        exclude_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        match self.is_unique(username, exclude_id).await {
            Ok(()) => Ok(true),
            Err(ApiError::Rule { code, .. })
                if code == crate::errors::api_error::codes::USERNAME_TAKEN =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    async fn exists(&self, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM users WHERE id = $1;")
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

    async fn mark_login(&self, user_id: Uuid) -> Result<bool, ApiError> {
        let is_first_login: bool = sqlx::query_scalar(
            r#"
            UPDATE users u
            SET last_login_at = $2
            FROM (SELECT last_login_at FROM users WHERE id = $1) AS old
            WHERE u.id = $1
            RETURNING old.last_login_at IS NULL
            "#,
        )
        .bind(user_id)
        .bind(now())
        .fetch_one(&self.db)
        .await
        .map_err(|e| {
            error!("Failed to record login for user {user_id}: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(is_first_login)
    }

    async fn get_username_history(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<UsernameHistoryEntry>, ApiError> {
        let history = sqlx::query_as::<_, UsernameHistoryEntry>(
            "SELECT old_username, changed_at FROM username_history
             WHERE user_id = $1
             ORDER BY changed_at ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        Ok(history)
    }

    async fn set_password(
        &self,
        id: Uuid,
        new_password: &str,
        must_change_password: bool,
        revoke_sessions: bool,
        actor_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let hash = encrypt_password(new_password)?;
        let timestamp = now();

        let result = sqlx::query(
            "UPDATE users
             SET password_hash = $1,
                 must_change_password = $2,
                 password_changed_at = $3,
                 token_version = token_version + CASE WHEN $4 THEN 1 ELSE 0 END,
                 updated_at = $3,
                 updated_by = COALESCE($5, updated_by)
             WHERE id = $6",
        )
        .bind(&hash)
        .bind(must_change_password)
        .bind(timestamp)
        .bind(revoke_sessions)
        .bind(actor_id)
        .bind(id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn set_must_change_password(&self, id: Uuid, value: bool) -> Result<(), ApiError> {
        sqlx::query("UPDATE users SET must_change_password = $1 WHERE id = $2")
            .bind(value)
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn revoke_sessions(&self, id: Uuid) -> Result<(), ApiError> {
        let result =
            sqlx::query("UPDATE users SET token_version = token_version + 1 WHERE id = $1")
                .bind(id)
                .execute(&self.db)
                .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn ban(
        &self,
        id: Uuid,
        until: Option<NaiveDateTime>,
        reason: Option<&str>,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let timestamp = now();
        // Bumping the token version signs the account out everywhere
        // immediately, instead of letting open sessions run until expiry.
        let result = sqlx::query(
            "UPDATE users
             SET banned_at = $1, banned_until = $2, ban_reason = $3, banned_by = $4,
                 token_version = token_version + 1, updated_at = $1, updated_by = $4
             WHERE id = $5",
        )
        .bind(timestamp)
        .bind(until)
        .bind(reason)
        .bind(actor_id)
        .bind(id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn unban(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE users
             SET banned_at = NULL, banned_until = NULL, ban_reason = NULL, banned_by = NULL,
                 updated_at = $1, updated_by = $2
             WHERE id = $3",
        )
        .bind(now())
        .bind(actor_id)
        .bind(id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn count_active_admins(&self) -> Result<i64, ApiError> {
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users
             WHERE role = 'admin' AND status = 'active'
               AND NOT (banned_at IS NOT NULL AND (banned_until IS NULL OR banned_until > (NOW() AT TIME ZONE 'utc')))",
        )
        .fetch_one(&self.db)
        .await?;
        Ok(count)
    }
}
