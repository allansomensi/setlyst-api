//! One-time codes, sign-in challenges, recovery codes and linked identity
//! providers.
//!
//! Plain secrets never reach this layer: codes arrive as HMACs and
//! challenge tokens as SHA-256 digests (see `utils::crypto`).

use crate::{
    errors::api_error::ApiError,
    models::security::{LinkedIdentity, LoginChallenge, VerificationCode, VerificationPurpose},
};
use chrono::{NaiveDateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Result of claiming a second-factor check outside sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactorClaim {
    /// The check may run; this many failures (this one included) are now
    /// counted in the window.
    Allowed { failures: i32 },
    /// Out of attempts until the window ends in this many seconds.
    Limited { retry_after_seconds: i64 },
}

#[async_trait::async_trait]
pub trait SecurityRepository: Send + Sync {
    // --- Verification codes ---

    /// Stores a new code, invalidating any unconsumed code of the same
    /// purpose for the user.
    async fn create_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        code_hash: &str,
        target_email: &str,
        expires_at: NaiveDateTime,
        ip: Option<&str>,
    ) -> Result<Uuid, ApiError>;
    /// The newest unconsumed code of `purpose` for the user.
    async fn latest_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
    ) -> Result<Option<VerificationCode>, ApiError>;
    /// When the newest code of `purpose` was issued, and how many were
    /// issued since `since` (rate limiting).
    async fn code_issuance(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        since: NaiveDateTime,
    ) -> Result<(Option<NaiveDateTime>, i64), ApiError>;
    /// Atomically claims one attempt on a code *before* it is checked:
    /// the new attempt count, or `None` when the code is consumed,
    /// expired or already out of attempts. Claiming first (in the same
    /// statement as the limit check) is what makes the limit hold under
    /// concurrent requests.
    async fn claim_code_attempt(
        &self,
        id: Uuid,
        max_attempts: i32,
    ) -> Result<Option<i32>, ApiError>;
    /// Gives back an attempt claimed by a correct code that couldn't be
    /// used yet (e.g. the new password was too weak), so the owner can
    /// retry. Never below zero.
    async fn release_code_attempt(&self, id: Uuid) -> Result<(), ApiError>;
    /// Marks a code consumed. `false` when it already was (lost race).
    async fn consume_code(&self, id: Uuid) -> Result<bool, ApiError>;

    // --- Two-factor sign-in challenges ---

    /// Stores a new challenge and invalidates every other unconsumed
    /// challenge of the user, so an account never has more than one live
    /// challenge (each fresh password sign-in can't add 5 more guesses).
    async fn create_challenge(
        &self,
        user_id: Uuid,
        token_hash: &str,
        method: &str,
        expires_at: NaiveDateTime,
        ip: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn find_challenge(&self, token_hash: &str) -> Result<Option<LoginChallenge>, ApiError>;
    /// Like [`SecurityRepository::claim_code_attempt`], for challenges.
    async fn claim_challenge_attempt(
        &self,
        id: Uuid,
        max_attempts: i32,
    ) -> Result<Option<i32>, ApiError>;
    /// Marks a challenge consumed. `false` when it already was.
    async fn consume_challenge(&self, id: Uuid) -> Result<bool, ApiError>;

    // --- Second-factor checks outside sign-in ---

    /// Claims one second-factor check for the user (disabling 2FA,
    /// regenerating recovery codes) in a fixed window of `window_seconds`
    /// that starts at the first failure: at most `max_failures` per
    /// window. Claimed before checking, atomically, like the code limits.
    async fn claim_second_factor_check(
        &self,
        user_id: Uuid,
        max_failures: i32,
        window_seconds: i64,
    ) -> Result<SecondFactorClaim, ApiError>;
    /// A correct code: the claimed check doesn't count as a failure.
    async fn release_second_factor_check(&self, user_id: Uuid) -> Result<(), ApiError>;

    // --- Recovery codes ---

    /// Replaces every recovery code of the user.
    async fn replace_recovery_codes(
        &self,
        user_id: Uuid,
        hashes: &[String],
    ) -> Result<(), ApiError>;
    /// Uses the unused code with `hash`. `false` when there is none.
    async fn use_recovery_code(&self, user_id: Uuid, hash: &str) -> Result<bool, ApiError>;
    async fn count_recovery_codes(&self, user_id: Uuid) -> Result<i64, ApiError>;
    async fn delete_recovery_codes(&self, user_id: Uuid) -> Result<(), ApiError>;

    // --- Identity providers ---

    async fn find_identity(&self, provider: &str, subject: &str) -> Result<Option<Uuid>, ApiError>;
    async fn link_identity(
        &self,
        user_id: Uuid,
        provider: &str,
        subject: &str,
        email: Option<&str>,
    ) -> Result<(), ApiError>;
    async fn touch_identity(&self, provider: &str, subject: &str) -> Result<(), ApiError>;
    async fn list_identities(&self, user_id: Uuid) -> Result<Vec<LinkedIdentity>, ApiError>;
    /// `false` when nothing was linked.
    async fn unlink_identity(&self, user_id: Uuid, provider: &str) -> Result<bool, ApiError>;

    // --- Maintenance ---

    /// Deletes codes and challenges that expired more than a day ago.
    async fn purge_expired(&self) -> Result<u64, ApiError>;
}

pub struct SecurityRepositoryImpl {
    pub db: PgPool,
}

impl SecurityRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl SecurityRepository for SecurityRepositoryImpl {
    async fn create_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        code_hash: &str,
        target_email: &str,
        expires_at: NaiveDateTime,
        ip: Option<&str>,
    ) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let timestamp = now();
        sqlx::query(
            "UPDATE verification_codes SET consumed_at = $3
             WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO verification_codes (id, user_id, purpose, code_hash, target_email, attempts,
                                             expires_at, ip_address, created_at)
             VALUES ($1, $2, $3, $4, $5, 0, $6, $7, $8)",
        )
        .bind(id)
        .bind(user_id)
        .bind(purpose)
        .bind(code_hash)
        .bind(target_email)
        .bind(expires_at)
        .bind(ip)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn latest_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
    ) -> Result<Option<VerificationCode>, ApiError> {
        Ok(sqlx::query_as::<_, VerificationCode>(
            "SELECT id, user_id, purpose, code_hash, target_email, attempts, expires_at, consumed_at, created_at
             FROM verification_codes
             WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(user_id)
        .bind(purpose)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn code_issuance(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        since: NaiveDateTime,
    ) -> Result<(Option<NaiveDateTime>, i64), ApiError> {
        let row: (Option<NaiveDateTime>, i64) = sqlx::query_as(
            "SELECT MAX(created_at), COUNT(*) FILTER (WHERE created_at >= $3)
             FROM verification_codes WHERE user_id = $1 AND purpose = $2",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(since)
        .fetch_one(&self.db)
        .await?;
        Ok(row)
    }

    async fn claim_code_attempt(
        &self,
        id: Uuid,
        max_attempts: i32,
    ) -> Result<Option<i32>, ApiError> {
        Ok(sqlx::query_scalar(
            "UPDATE verification_codes SET attempts = attempts + 1
             WHERE id = $1 AND consumed_at IS NULL AND attempts < $2 AND expires_at > $3
             RETURNING attempts",
        )
        .bind(id)
        .bind(max_attempts)
        .bind(now())
        .fetch_optional(&self.db)
        .await?)
    }

    async fn release_code_attempt(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE verification_codes SET attempts = GREATEST(attempts - 1, 0)
             WHERE id = $1 AND consumed_at IS NULL",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn consume_code(&self, id: Uuid) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "UPDATE verification_codes SET consumed_at = $2 WHERE id = $1 AND consumed_at IS NULL",
        )
        .bind(id)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn create_challenge(
        &self,
        user_id: Uuid,
        token_hash: &str,
        method: &str,
        expires_at: NaiveDateTime,
        ip: Option<&str>,
    ) -> Result<(), ApiError> {
        let timestamp = now();
        let mut tx = self.db.begin().await?;
        // Serializes concurrent sign-ins of the same account, so two
        // parallel password checks can't both leave a live challenge.
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE login_challenges SET consumed_at = $2
             WHERE user_id = $1 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO login_challenges (id, user_id, token_hash, method, attempts, expires_at,
                                           ip_address, created_at)
             VALUES ($1, $2, $3, $4, 0, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(token_hash)
        .bind(method)
        .bind(expires_at)
        .bind(ip)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn find_challenge(&self, token_hash: &str) -> Result<Option<LoginChallenge>, ApiError> {
        Ok(sqlx::query_as::<_, LoginChallenge>(
            "SELECT id, user_id, method, attempts, expires_at, consumed_at
             FROM login_challenges WHERE token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn claim_challenge_attempt(
        &self,
        id: Uuid,
        max_attempts: i32,
    ) -> Result<Option<i32>, ApiError> {
        Ok(sqlx::query_scalar(
            "UPDATE login_challenges SET attempts = attempts + 1
             WHERE id = $1 AND consumed_at IS NULL AND attempts < $2 AND expires_at > $3
             RETURNING attempts",
        )
        .bind(id)
        .bind(max_attempts)
        .bind(now())
        .fetch_optional(&self.db)
        .await?)
    }

    async fn consume_challenge(&self, id: Uuid) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "UPDATE login_challenges SET consumed_at = $2 WHERE id = $1 AND consumed_at IS NULL",
        )
        .bind(id)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn claim_second_factor_check(
        &self,
        user_id: Uuid,
        max_failures: i32,
        window_seconds: i64,
    ) -> Result<SecondFactorClaim, ApiError> {
        let timestamp = now();
        let window_start = timestamp - chrono::Duration::seconds(window_seconds);
        // One statement: a new window starts when the previous one is
        // over; otherwise the counter only moves while below the limit
        // (the `WHERE` of the upsert), so concurrent checks can't overshoot.
        let claimed: Option<i32> = sqlx::query_scalar(
            "INSERT INTO second_factor_attempts (user_id, window_started_at, failures)
             VALUES ($1, $2, 1)
             ON CONFLICT (user_id) DO UPDATE SET
                 failures = CASE WHEN second_factor_attempts.window_started_at <= $3
                                 THEN 1 ELSE second_factor_attempts.failures + 1 END,
                 window_started_at = CASE WHEN second_factor_attempts.window_started_at <= $3
                                          THEN $2 ELSE second_factor_attempts.window_started_at END
             WHERE second_factor_attempts.window_started_at <= $3
                OR second_factor_attempts.failures < $4
             RETURNING failures",
        )
        .bind(user_id)
        .bind(timestamp)
        .bind(window_start)
        .bind(max_failures)
        .fetch_optional(&self.db)
        .await?;
        if let Some(failures) = claimed {
            return Ok(SecondFactorClaim::Allowed { failures });
        }
        let started: Option<NaiveDateTime> = sqlx::query_scalar(
            "SELECT window_started_at FROM second_factor_attempts WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        let retry_after_seconds = started
            .map(|s| (s + chrono::Duration::seconds(window_seconds) - timestamp).num_seconds())
            .unwrap_or(window_seconds)
            .max(1);
        Ok(SecondFactorClaim::Limited {
            retry_after_seconds,
        })
    }

    async fn release_second_factor_check(&self, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE second_factor_attempts SET failures = GREATEST(failures - 1, 0)
             WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn replace_recovery_codes(
        &self,
        user_id: Uuid,
        hashes: &[String],
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let ids: Vec<Uuid> = hashes.iter().map(|_| Uuid::now_v7()).collect();
        sqlx::query(
            "INSERT INTO totp_recovery_codes (id, user_id, code_hash, created_at)
             SELECT t.id, $2, t.hash, $4 FROM UNNEST($1::uuid[], $3::text[]) AS t(id, hash)",
        )
        .bind(&ids)
        .bind(user_id)
        .bind(hashes)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn use_recovery_code(&self, user_id: Uuid, hash: &str) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "UPDATE totp_recovery_codes SET used_at = $3
             WHERE id = (SELECT id FROM totp_recovery_codes
                         WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL
                         LIMIT 1 FOR UPDATE)
               AND used_at IS NULL",
        )
        .bind(user_id)
        .bind(hash)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn count_recovery_codes(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM totp_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn delete_recovery_codes(&self, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM totp_recovery_codes WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn find_identity(&self, provider: &str, subject: &str) -> Result<Option<Uuid>, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT user_id FROM oauth_identities WHERE provider = $1 AND subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn link_identity(
        &self,
        user_id: Uuid,
        provider: &str,
        subject: &str,
        email: Option<&str>,
    ) -> Result<(), ApiError> {
        let timestamp = now();
        sqlx::query(
            "INSERT INTO oauth_identities (id, user_id, provider, subject, email, created_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(provider)
        .bind(subject)
        .bind(email)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn touch_identity(&self, provider: &str, subject: &str) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE oauth_identities SET last_used_at = $3 WHERE provider = $1 AND subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn list_identities(&self, user_id: Uuid) -> Result<Vec<LinkedIdentity>, ApiError> {
        Ok(sqlx::query_as::<_, LinkedIdentity>(
            "SELECT provider, email, created_at, last_used_at
             FROM oauth_identities WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?)
    }

    async fn unlink_identity(&self, user_id: Uuid, provider: &str) -> Result<bool, ApiError> {
        let result =
            sqlx::query("DELETE FROM oauth_identities WHERE user_id = $1 AND provider = $2")
                .bind(user_id)
                .bind(provider)
                .execute(&self.db)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn purge_expired(&self) -> Result<u64, ApiError> {
        let cutoff = now() - chrono::Duration::days(1);
        let codes = sqlx::query("DELETE FROM verification_codes WHERE expires_at < $1")
            .bind(cutoff)
            .execute(&self.db)
            .await?
            .rows_affected();
        let challenges = sqlx::query("DELETE FROM login_challenges WHERE expires_at < $1")
            .bind(cutoff)
            .execute(&self.db)
            .await?
            .rows_affected();
        Ok(codes + challenges)
    }
}
