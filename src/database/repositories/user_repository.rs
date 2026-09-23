use crate::{
    errors::api_error::{ApiError, codes},
    models::user::{
        BandInCommon, CreateUserPayload, ProfileUpdate, Role, Status, UpdateUserPayload, User,
        UserPublic, UsernameHistoryEntry, clearable,
    },
    utils::{codes::referral_code, hashing::hash_password},
};
use axum::http::StatusCode;
use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
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
         u.created_at, u.updated_at,
         (u.email_verified_at IS NOT NULL AND u.email IS NOT NULL) AS email_verified,
         u.avatar_url, u.bio, u.location, u.instruments,
         (u.totp_enabled_at IS NOT NULL AND u.totp_secret_enc IS NOT NULL) AS two_factor_enabled,
         u.password_set, u.terms_version, u.terms_accepted_at, u.referral_code"
    };
}

/// Columns selected for a full [`User`] (including secrets) from `users`.
macro_rules! user_account_columns {
    () => {
        "id, username, email, password_hash, first_name, last_name, role, status,
         token_version, must_change_password, banned_at, banned_until, ban_reason,
         username_changed_at, created_at, updated_at, email_verified_at, password_set,
         totp_secret_enc, totp_enabled_at, totp_pending_secret_enc, totp_pending_created_at,
         totp_last_step, failed_login_count, locked_until, terms_accepted_at, terms_version,
         referral_code, avatar_url"
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

    // --- v0.12: accounts and security ---

    /// Full account row by e-mail, case-insensitively.
    async fn find_by_email(&self, email: &str) -> Result<Option<User>, ApiError>;
    /// Full account row by username or e-mail (an `@` means e-mail).
    async fn find_by_identifier(&self, identifier: &str) -> Result<Option<User>, ApiError>;
    /// `true` when another account (not `exclude_id`) uses `email`,
    /// case-insensitively.
    async fn is_email_taken(&self, email: &str, exclude_id: Option<Uuid>)
    -> Result<bool, ApiError>;
    async fn find_by_referral_code(&self, code: &str) -> Result<Option<Uuid>, ApiError>;
    /// Creates a self-registered (or identity-provider) account with its
    /// preferences and referral record, in one transaction.
    async fn register(&self, account: &NewAccount) -> Result<User, ApiError>;
    /// Counts a failed sign-in and locks the account after every
    /// `threshold` consecutive failures (15 min, doubling, max 24 h).
    /// Returns the new failure count and the lock, if one was set now.
    async fn record_failed_login(
        &self,
        id: Uuid,
        threshold: i32,
    ) -> Result<(i32, Option<NaiveDateTime>), ApiError>;
    async fn reset_login_failures(&self, id: Uuid) -> Result<(), ApiError>;
    /// Password recovery: sets the password, revokes sessions, clears the
    /// lockout and (when `verify_email`) marks the e-mail verified.
    async fn recover_password(
        &self,
        id: Uuid,
        new_password: &str,
        verify_email: bool,
    ) -> Result<(), ApiError>;
    /// Marks `email` verified, provided it is still the account's address.
    async fn mark_email_verified(&self, id: Uuid, email: &str) -> Result<bool, ApiError>;
    /// Replaces the e-mail with an already verified address.
    async fn change_email(&self, id: Uuid, new_email: &str) -> Result<(), ApiError>;
    async fn accept_terms(&self, id: Uuid, version: &str) -> Result<(), ApiError>;
    async fn update_profile(&self, id: Uuid, update: &ProfileUpdate) -> Result<(), ApiError>;
    async fn bands_in_common(&self, a: Uuid, b: Uuid) -> Result<Vec<BandInCommon>, ApiError>;
    async fn set_pending_totp(&self, id: Uuid, secret_enc: &str) -> Result<(), ApiError>;
    /// Promotes the pending secret; `step` is the TOTP step just used.
    async fn enable_totp(&self, id: Uuid, step: i64) -> Result<(), ApiError>;
    async fn disable_totp(&self, id: Uuid) -> Result<(), ApiError>;
    /// Records `step` as used. `false` when it (or a later one) already
    /// was: the code is a replay.
    async fn claim_totp_step(&self, id: Uuid, step: i64) -> Result<bool, ApiError>;
    /// Moderation: replaces the username (recording history) and lets the
    /// owner rename right away.
    async fn force_username(
        &self,
        id: Uuid,
        username: &str,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;
    async fn remove_avatar(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
}

/// A new account created by self-registration or an identity provider.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub password_set: bool,
    pub email_verified: bool,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub terms_version: Option<String>,
    pub referred_by: Option<Uuid>,
    pub language: String,
    /// Initial `user_preferences.communication`.
    pub communication: Value,
}

/// Inserts a user row with a fresh, unique referral code (retrying on the
/// rare collision).
async fn insert_with_referral_code(
    tx: &mut Transaction<'_, Postgres>,
    user: &User,
    created_by: Option<Uuid>,
) -> Result<String, ApiError> {
    for _ in 0..5 {
        let code = referral_code();
        let result = sqlx::query(
            r#"INSERT INTO users (id, username, email, password_hash, first_name, last_name, role, status,
                                  must_change_password, password_changed_at, created_by, created_at, updated_at,
                                  referral_code, password_set, email_verified_at, terms_accepted_at,
                                  terms_version, referred_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
            ON CONFLICT (referral_code) DO NOTHING"#,
        )
        .bind(user.id)
        .bind(&user.username)
        .bind(&user.email)
        .bind(&user.password_hash)
        .bind(&user.first_name)
        .bind(&user.last_name)
        .bind(&user.role)
        .bind(&user.status)
        .bind(user.must_change_password)
        .bind(user.password_set.then_some(user.created_at))
        .bind(created_by)
        .bind(user.created_at)
        .bind(user.updated_at)
        .bind(&code)
        .bind(user.password_set)
        .bind(user.email_verified_at)
        .bind(user.terms_accepted_at)
        .bind(&user.terms_version)
        .bind(None::<Uuid>)
        .execute(&mut **tx)
        .await?;
        if result.rows_affected() == 1 {
            return Ok(code);
        }
    }
    Err(ApiError::ServerError(axum::Error::new(
        "could not allocate a unique referral code",
    )))
}

/// Refuses to leave the platform without an active admin. Locks the admin
/// rows so two concurrent demotions can't both pass the check.
async fn guard_last_admin(
    tx: &mut Transaction<'_, Postgres>,
    target: Uuid,
) -> Result<(), ApiError> {
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users
         WHERE role = 'admin' AND status = 'active'
           AND NOT (banned_at IS NOT NULL AND (banned_until IS NULL OR banned_until > (NOW() AT TIME ZONE 'utc')))
         FOR UPDATE",
    )
    .fetch_all(&mut **tx)
    .await?;
    if admins.contains(&target) && admins.len() <= 1 {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::LAST_ADMIN,
            "The platform must keep at least one active admin.",
        ));
    }
    Ok(())
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
            hash_password(&payload.password).await?.as_str(),
            first_name,
            last_name,
            payload.role.clone(),
            payload.status.clone(),
        );
        new_user.must_change_password = must_change_password;

        let mut tx = self.db.begin().await?;
        new_user.referral_code =
            Some(insert_with_referral_code(&mut tx, &new_user, created_by).await?);
        tx.commit().await?;

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
            // A staff-set address is unproven: it must be verified again,
            // unless it's the same address as before.
            sqlx::query(
                "UPDATE users
                 SET email_verified_at = CASE WHEN LOWER(COALESCE(email, '')) = LOWER(COALESCE($1, ''))
                                              THEN email_verified_at ELSE NULL END,
                     email = $1
                 WHERE id = $2",
            )
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
            if *role != Role::Admin {
                guard_last_admin(&mut tx, id).await?;
            }
            sqlx::query("UPDATE users SET role = $1 WHERE id = $2;")
                .bind(role)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(status) = &payload.status {
            if *status != Status::Active {
                guard_last_admin(&mut tx, id).await?;
            }
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

        // Self-service deletion of the last admin would leave the platform
        // without anyone able to administer it.
        guard_last_admin(&mut tx, id).await?;

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

        // Band tours (live or in the trash) follow the same rule. Personal
        // content, trashed or not, goes away with the account.
        sqlx::query(
            "UPDATE tours t SET user_id = bm.user_id
             FROM band_members bm
             WHERE t.user_id = $1 AND t.band_id IS NOT NULL
               AND bm.band_id = t.band_id AND bm.role = 'owner' AND bm.user_id != $1",
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
        let hash = hash_password(new_password).await?;
        let timestamp = now();

        let result = sqlx::query(
            "UPDATE users
             SET password_hash = $1,
                 password_set = TRUE,
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

    async fn find_by_email(&self, email: &str) -> Result<Option<User>, ApiError> {
        // `LIMIT 1` + ordering: older databases may still hold addresses
        // that differ only by case (the unique index is then missing);
        // the oldest account wins.
        let user = sqlx::query_as::<_, User>(concat!(
            "SELECT ",
            user_account_columns!(),
            " FROM users WHERE LOWER(email) = LOWER($1) ORDER BY created_at LIMIT 1"
        ))
        .bind(email.trim())
        .fetch_optional(&self.db)
        .await?;
        Ok(user)
    }

    async fn find_by_identifier(&self, identifier: &str) -> Result<Option<User>, ApiError> {
        let identifier = identifier.trim();
        if identifier.contains('@') {
            self.find_by_email(identifier).await
        } else {
            self.find_by_username(identifier).await
        }
    }

    async fn is_email_taken(
        &self,
        email: &str,
        exclude_id: Option<Uuid>,
    ) -> Result<bool, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM users
                            WHERE LOWER(email) = LOWER($1) AND ($2::uuid IS NULL OR id != $2))",
        )
        .bind(email.trim())
        .bind(exclude_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn find_by_referral_code(&self, code: &str) -> Result<Option<Uuid>, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT id FROM users WHERE referral_code = UPPER($1)")
                .bind(code.trim())
                .fetch_optional(&self.db)
                .await?,
        )
    }

    async fn register(&self, account: &NewAccount) -> Result<User, ApiError> {
        let timestamp = now();
        let mut user = User::new(
            account.username.trim(),
            Some(account.email.trim().to_string()),
            &account.password_hash,
            account.first_name.clone(),
            account.last_name.clone(),
            Some(Role::User),
            Some(Status::Active),
        );
        user.password_set = account.password_set;
        user.email_verified_at = account.email_verified.then_some(timestamp);
        user.terms_version = account.terms_version.clone();
        user.terms_accepted_at = account.terms_version.as_ref().map(|_| timestamp);

        let mut tx = self.db.begin().await?;
        user.referral_code = Some(insert_with_referral_code(&mut tx, &user, None).await?);

        if let Some(referrer) = account.referred_by.filter(|r| *r != user.id) {
            sqlx::query("UPDATE users SET referred_by = $2 WHERE id = $1")
                .bind(user.id)
                .bind(referrer)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO referrals (id, referrer_id, referred_id, status, created_at)
                 VALUES ($1, $2, $3, 'pending', $4)",
            )
            .bind(Uuid::now_v7())
            .bind(referrer)
            .bind(user.id)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT INTO user_preferences (id, user_id, language, theme, live_mode_font_size, ui_settings,
                                           communication, created_at, updated_at)
             VALUES ($1, $2, $3, 'system', 100, '{}', $4, $5, $5)
             ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(&account.language)
        .bind(&account.communication)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(user)
    }

    async fn record_failed_login(
        &self,
        id: Uuid,
        threshold: i32,
    ) -> Result<(i32, Option<NaiveDateTime>), ApiError> {
        let timestamp = now();
        // One statement, so concurrent failures can't lose increments.
        // Every `threshold` failures lock the account for 15 minutes,
        // doubling with each further lock, capped at 24 hours.
        let row: (i32, Option<NaiveDateTime>) = sqlx::query_as(
            "UPDATE users
             SET failed_login_count = failed_login_count + 1,
                 locked_until = CASE
                     WHEN (failed_login_count + 1) % $2 = 0
                     THEN $3 + LEAST(
                         INTERVAL '24 hours',
                         INTERVAL '15 minutes' * POWER(2, LEAST(((failed_login_count + 1) / $2) - 1, 10))
                     )
                     ELSE locked_until
                 END
             WHERE id = $1
             RETURNING failed_login_count, locked_until",
        )
        .bind(id)
        .bind(threshold)
        .bind(timestamp)
        .fetch_one(&self.db)
        .await?;
        let lock = row
            .1
            .filter(|until| *until > timestamp && row.0 % threshold == 0);
        Ok((row.0, lock))
    }

    async fn reset_login_failures(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE users SET failed_login_count = 0, locked_until = NULL
             WHERE id = $1 AND (failed_login_count <> 0 OR locked_until IS NOT NULL)",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn recover_password(
        &self,
        id: Uuid,
        new_password: &str,
        verify_email: bool,
    ) -> Result<(), ApiError> {
        let hash = hash_password(new_password).await?;
        let timestamp = now();
        let result = sqlx::query(
            "UPDATE users
             SET password_hash = $1, password_set = TRUE, must_change_password = FALSE,
                 password_changed_at = $2, token_version = token_version + 1,
                 failed_login_count = 0, locked_until = NULL,
                 email_verified_at = CASE WHEN $3 THEN COALESCE(email_verified_at, $2) ELSE email_verified_at END,
                 updated_at = $2
             WHERE id = $4",
        )
        .bind(&hash)
        .bind(timestamp)
        .bind(verify_email)
        .bind(id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn mark_email_verified(&self, id: Uuid, email: &str) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "UPDATE users SET email_verified_at = $3
             WHERE id = $1 AND LOWER(email) = LOWER($2) AND email_verified_at IS NULL",
        )
        .bind(id)
        .bind(email)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn change_email(&self, id: Uuid, new_email: &str) -> Result<(), ApiError> {
        let timestamp = now();
        sqlx::query(
            "UPDATE users SET email = $2, email_verified_at = $3, updated_at = $3, updated_by = $1
             WHERE id = $1",
        )
        .bind(id)
        .bind(new_email.trim())
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn accept_terms(&self, id: Uuid, version: &str) -> Result<(), ApiError> {
        sqlx::query("UPDATE users SET terms_version = $2, terms_accepted_at = $3 WHERE id = $1")
            .bind(id)
            .bind(version)
            .bind(now())
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn update_profile(&self, id: Uuid, update: &ProfileUpdate) -> Result<(), ApiError> {
        let timestamp = now();
        // `$n IS TRUE` flags say which fields to write, so one statement
        // covers every combination without building SQL at runtime.
        sqlx::query(
            "UPDATE users SET
                 bio = CASE WHEN $2 THEN $3 ELSE bio END,
                 location = CASE WHEN $4 THEN $5 ELSE location END,
                 instruments = CASE WHEN $6 THEN $7 ELSE instruments END,
                 avatar_updated_at = CASE WHEN $8 AND avatar_url IS DISTINCT FROM $9 THEN $10 ELSE avatar_updated_at END,
                 avatar_url = CASE WHEN $8 THEN $9 ELSE avatar_url END,
                 updated_at = $10, updated_by = $1
             WHERE id = $1",
        )
        .bind(id)
        .bind(update.bio.is_some())
        .bind(update.bio.clone().flatten())
        .bind(update.location.is_some())
        .bind(update.location.clone().flatten())
        .bind(update.instruments.is_some())
        .bind(update.instruments.clone().unwrap_or_default())
        .bind(update.avatar_url.is_some())
        .bind(update.avatar_url.clone().flatten())
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn bands_in_common(&self, a: Uuid, b: Uuid) -> Result<Vec<BandInCommon>, ApiError> {
        Ok(sqlx::query_as::<_, BandInCommon>(
            "SELECT bd.id, bd.name, bd.logo_url
             FROM bands bd
             JOIN band_members ma ON ma.band_id = bd.id AND ma.user_id = $1
             JOIN band_members mb ON mb.band_id = bd.id AND mb.user_id = $2
             ORDER BY LOWER(bd.name)
             LIMIT 50",
        )
        .bind(a)
        .bind(b)
        .fetch_all(&self.db)
        .await?)
    }

    async fn set_pending_totp(&self, id: Uuid, secret_enc: &str) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE users SET totp_pending_secret_enc = $2, totp_pending_created_at = $3 WHERE id = $1",
        )
        .bind(id)
        .bind(secret_enc)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn enable_totp(&self, id: Uuid, step: i64) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE users
             SET totp_secret_enc = totp_pending_secret_enc, totp_enabled_at = $2, totp_last_step = $3,
                 totp_pending_secret_enc = NULL, totp_pending_created_at = NULL
             WHERE id = $1 AND totp_pending_secret_enc IS NOT NULL",
        )
        .bind(id)
        .bind(now())
        .bind(step)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn disable_totp(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE users
             SET totp_secret_enc = NULL, totp_enabled_at = NULL, totp_last_step = NULL,
                 totp_pending_secret_enc = NULL, totp_pending_created_at = NULL
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn claim_totp_step(&self, id: Uuid, step: i64) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "UPDATE users SET totp_last_step = $2
             WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)",
        )
        .bind(id)
        .bind(step)
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn force_username(
        &self,
        id: Uuid,
        username: &str,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let previous: Option<String> =
            sqlx::query_scalar("SELECT username FROM users WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(previous) = previous else {
            return Err(ApiError::NotFound);
        };
        let timestamp = now();
        sqlx::query(
            "UPDATE users SET username = $2, username_changed_at = NULL, updated_at = $3, updated_by = $4
             WHERE id = $1",
        )
        .bind(id)
        .bind(username)
        .bind(timestamp)
        .bind(actor_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO username_history (id, user_id, old_username, changed_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(&previous)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn remove_avatar(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let timestamp = now();
        sqlx::query(
            "UPDATE users SET avatar_url = NULL, avatar_updated_at = $2, updated_at = $2, updated_by = $3
             WHERE id = $1",
        )
        .bind(id)
        .bind(timestamp)
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }
}
