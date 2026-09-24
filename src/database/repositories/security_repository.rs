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

/// Issuance limits of an e-mailed code, checked in the same transaction
/// as the insert (with the account row locked), so concurrent requests
/// can't all pass the resend interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeLimits {
    /// Minimum seconds between two requests of the purpose for the
    /// account, whoever makes them.
    pub resend_seconds: i64,
    /// Requests are counted over `window_seconds`...
    pub window_seconds: i64,
    /// ...at most this many per requester, and...
    pub max_per_requester_in_window: i64,
    /// ...this many for the account altogether (what bounds the mail the
    /// owner's inbox can receive because of strangers' requests). Must be
    /// at least `max_per_requester_in_window`.
    pub max_in_window: i64,
    /// No new code once this many wrong guesses were made on the
    /// purpose's codes in the last 24 hours.
    pub max_failures_per_day: i64,
}

/// Outcome of [`SecurityRepository::create_code`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeIssue {
    /// A new code was stored.
    Issued(Uuid),
    /// A code for the same purpose and address is still live, so nothing
    /// new was issued: send `code_enc` (the stored code, encrypted) again.
    Resent {
        id: Uuid,
        code_enc: String,
        expires_at: NaiveDateTime,
    },
    /// Refused by a limit; retry after this many seconds.
    Limited { retry_after_seconds: i64 },
}

/// The requester key of a code request when the client network is
/// unknown.
pub const UNKNOWN_REQUESTER: &str = "unknown";

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

    /// Records a request for a code of `purpose` by `requester` (the
    /// client network, or [`UNKNOWN_REQUESTER`]) and either stores the new
    /// code (invalidating any unconsumed code of the same purpose for the
    /// user) or, when a code for the same purpose and `target_email` is
    /// still valid with at least `min_remaining` of its lifetime left,
    /// answers [`CodeIssue::Resent`] with that code so the caller sends it
    /// again: a stranger's request can then never invalidate the code the
    /// owner is about to type. `limits` are checked with the account row
    /// locked, in the same transaction.
    #[allow(clippy::too_many_arguments)]
    async fn create_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        code_hash: &str,
        code_enc: &str,
        target_email: &str,
        expires_at: NaiveDateTime,
        min_remaining: chrono::Duration,
        ip: Option<&str>,
        requester: &str,
        limits: CodeLimits,
    ) -> Result<CodeIssue, ApiError>;
    /// Counts a wrong guess on a code (for the 24-hour cap).
    async fn record_code_failure(&self, id: Uuid) -> Result<(), ApiError>;
    /// Wrong guesses on the user's codes of `purpose` since `since`.
    async fn code_failures_since(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError>;
    /// The newest unconsumed code of `purpose` for the user.
    async fn latest_code(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
    ) -> Result<Option<VerificationCode>, ApiError>;
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
    /// `pending_link` is a Google identity (`sub`, e-mail) to link once
    /// the challenge's second factor succeeds.
    async fn create_challenge(
        &self,
        user_id: Uuid,
        token_hash: &str,
        method: &str,
        expires_at: NaiveDateTime,
        ip: Option<&str>,
        pending_link: Option<(&str, &str)>,
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

    // --- Re-authentication (password or e-mailed code while signed in) ---

    /// Like [`SecurityRepository::claim_second_factor_check`], for
    /// re-authentication proofs.
    async fn claim_reauth_check(
        &self,
        user_id: Uuid,
        max_failures: i32,
        window_seconds: i64,
    ) -> Result<SecondFactorClaim, ApiError>;
    /// A correct proof: the claimed check doesn't count as a failure.
    async fn release_reauth_check(&self, user_id: Uuid) -> Result<(), ApiError>;
    /// Counts a wrong proof in the 24-hour window; returns the failures in
    /// the window (this one included).
    async fn record_reauth_failure(&self, user_id: Uuid) -> Result<i32, ApiError>;

    // --- Trials ---

    /// Claims the trial of the (hashed, canonical) address. `false` when
    /// it was already claimed, by any account, ever.
    async fn claim_trial(&self, email_hash: &[u8]) -> Result<bool, ApiError>;
    /// Gives a claim back (no trial was actually started).
    async fn release_trial_claim(&self, email_hash: &[u8]) -> Result<(), ApiError>;

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
        code_enc: &str,
        target_email: &str,
        expires_at: NaiveDateTime,
        min_remaining: chrono::Duration,
        ip: Option<&str>,
        requester: &str,
        limits: CodeLimits,
    ) -> Result<CodeIssue, ApiError> {
        let mut tx = self.db.begin().await?;
        let timestamp = now();
        // Serializes issuance per account: concurrent requests all see
        // the code the first one inserted.
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let window_start = timestamp - chrono::Duration::seconds(limits.window_seconds);
        // The resend interval is per account (one e-mail a minute, from
        // anyone); the allowance is per requester, so one stranger can't
        // use up the owner's, under an account-wide ceiling that bounds
        // the mail the owner's inbox can receive from strangers.
        let (last, by_requester, total): (Option<NaiveDateTime>, i64, i64) = sqlx::query_as(
            "SELECT MAX(created_at),
                    COUNT(*) FILTER (WHERE requester = $3),
                    COUNT(*)
             FROM verification_code_requests
             WHERE user_id = $1 AND purpose = $2 AND created_at >= $4",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(requester)
        .bind(window_start)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(last) = last {
            let wait = limits.resend_seconds - (timestamp - last).num_seconds();
            if wait > 0 {
                return Ok(CodeIssue::Limited {
                    retry_after_seconds: wait,
                });
            }
        }
        if by_requester >= limits.max_per_requester_in_window || total >= limits.max_in_window {
            return Ok(CodeIssue::Limited {
                retry_after_seconds: limits.window_seconds.min(3600),
            });
        }
        let failures: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(failed_attempts), 0)::BIGINT FROM verification_codes
             WHERE user_id = $1 AND purpose = $2 AND created_at >= $3",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(timestamp - chrono::Duration::hours(24))
        .fetch_one(&mut *tx)
        .await?;
        if failures >= limits.max_failures_per_day {
            return Ok(CodeIssue::Limited {
                retry_after_seconds: 3600,
            });
        }
        sqlx::query(
            "INSERT INTO verification_code_requests (id, user_id, purpose, requester, created_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(purpose)
        .bind(requester)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;

        // A live code for the same address, with enough life left to be
        // typed, is sent again rather than replaced.
        let live: Option<(Uuid, String, NaiveDateTime)> = sqlx::query_as(
            "SELECT id, code_enc, expires_at FROM verification_codes
             WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL
               AND code_enc IS NOT NULL AND LOWER(target_email) = LOWER($3)
               AND expires_at >= $4
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(target_email)
        .bind(timestamp + min_remaining)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id, code_enc, expires_at)) = live {
            tx.commit().await?;
            return Ok(CodeIssue::Resent {
                id,
                code_enc,
                expires_at,
            });
        }

        sqlx::query(
            "UPDATE verification_codes SET consumed_at = $3, code_enc = NULL
             WHERE user_id = $1 AND purpose = $2 AND consumed_at IS NULL",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO verification_codes (id, user_id, purpose, code_hash, code_enc, target_email,
                                             attempts, expires_at, ip_address, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, 0, $7, $8, $9)",
        )
        .bind(id)
        .bind(user_id)
        .bind(purpose)
        .bind(code_hash)
        .bind(code_enc)
        .bind(target_email)
        .bind(expires_at)
        .bind(ip)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CodeIssue::Issued(id))
    }

    async fn record_code_failure(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE verification_codes SET failed_attempts = failed_attempts + 1 WHERE id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn code_failures_since(
        &self,
        user_id: Uuid,
        purpose: VerificationPurpose,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COALESCE(SUM(failed_attempts), 0)::BIGINT FROM verification_codes
             WHERE user_id = $1 AND purpose = $2 AND created_at >= $3",
        )
        .bind(user_id)
        .bind(purpose)
        .bind(since)
        .fetch_one(&self.db)
        .await?)
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
            "UPDATE verification_codes SET consumed_at = $2, code_enc = NULL
             WHERE id = $1 AND consumed_at IS NULL",
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
        pending_link: Option<(&str, &str)>,
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
                                           ip_address, created_at, pending_link_subject,
                                           pending_link_email)
             VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8, $9)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(token_hash)
        .bind(method)
        .bind(expires_at)
        .bind(ip)
        .bind(timestamp)
        .bind(pending_link.map(|(subject, _)| subject))
        .bind(pending_link.map(|(_, email)| email))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn find_challenge(&self, token_hash: &str) -> Result<Option<LoginChallenge>, ApiError> {
        Ok(sqlx::query_as::<_, LoginChallenge>(
            "SELECT id, user_id, method, attempts, expires_at, consumed_at, created_at,
                    pending_link_subject, pending_link_email
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

    async fn claim_reauth_check(
        &self,
        user_id: Uuid,
        max_failures: i32,
        window_seconds: i64,
    ) -> Result<SecondFactorClaim, ApiError> {
        let timestamp = now();
        let window_start = timestamp - chrono::Duration::seconds(window_seconds);
        // Same fixed-window upsert as the second-factor limiter.
        let claimed: Option<i32> = sqlx::query_scalar(
            "INSERT INTO reauth_attempts (user_id, window_started_at, failures, day_started_at, day_failures)
             VALUES ($1, $2, 1, $2, 0)
             ON CONFLICT (user_id) DO UPDATE SET
                 failures = CASE WHEN reauth_attempts.window_started_at <= $3
                                 THEN 1 ELSE reauth_attempts.failures + 1 END,
                 window_started_at = CASE WHEN reauth_attempts.window_started_at <= $3
                                          THEN $2 ELSE reauth_attempts.window_started_at END
             WHERE reauth_attempts.window_started_at <= $3
                OR reauth_attempts.failures < $4
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
        let started: Option<NaiveDateTime> =
            sqlx::query_scalar("SELECT window_started_at FROM reauth_attempts WHERE user_id = $1")
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

    async fn release_reauth_check(&self, user_id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE reauth_attempts SET failures = GREATEST(failures - 1, 0) WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn record_reauth_failure(&self, user_id: Uuid) -> Result<i32, ApiError> {
        let timestamp = now();
        let day_start = timestamp - chrono::Duration::hours(24);
        Ok(sqlx::query_scalar(
            "INSERT INTO reauth_attempts (user_id, window_started_at, failures, day_started_at, day_failures)
             VALUES ($1, $2, 0, $2, 1)
             ON CONFLICT (user_id) DO UPDATE SET
                 day_failures = CASE WHEN reauth_attempts.day_started_at <= $3
                                     THEN 1 ELSE reauth_attempts.day_failures + 1 END,
                 day_started_at = CASE WHEN reauth_attempts.day_started_at <= $3
                                       THEN $2 ELSE reauth_attempts.day_started_at END
             RETURNING day_failures",
        )
        .bind(user_id)
        .bind(timestamp)
        .bind(day_start)
        .fetch_one(&self.db)
        .await?)
    }

    async fn claim_trial(&self, email_hash: &[u8]) -> Result<bool, ApiError> {
        let result = sqlx::query(
            "INSERT INTO trial_claims (email_hash, claimed_at) VALUES ($1, $2)
             ON CONFLICT (email_hash) DO NOTHING",
        )
        .bind(email_hash)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn release_trial_claim(&self, email_hash: &[u8]) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM trial_claims WHERE email_hash = $1")
            .bind(email_hash)
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
        // Requests only matter for their window (a day at most).
        sqlx::query("DELETE FROM verification_code_requests WHERE created_at < $1")
            .bind(cutoff)
            .execute(&self.db)
            .await?;
        // A live code that expired keeps its hash for the failure count,
        // never the code itself.
        sqlx::query(
            "UPDATE verification_codes SET code_enc = NULL
             WHERE code_enc IS NOT NULL AND (expires_at < $1 OR consumed_at IS NOT NULL)",
        )
        .bind(now())
        .execute(&self.db)
        .await?;
        let challenges = sqlx::query("DELETE FROM login_challenges WHERE expires_at < $1")
            .bind(cutoff)
            .execute(&self.db)
            .await?
            .rows_affected();
        Ok(codes + challenges)
    }
}
