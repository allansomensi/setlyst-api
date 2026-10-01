use crate::{
    errors::api_error::ApiError,
    models::console::{
        ConsoleSearchResults, DailyCount, SearchBand, SearchContent, SearchTicket, SearchUser,
        SignInEvent, UserCounts,
    },
};
use chrono::{Duration, NaiveDateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Matches of each kind a console search returns.
pub const SEARCH_LIMIT: i64 = 5;

#[async_trait::async_trait]
pub trait ConsoleRepository: Send + Sync {
    async fn user_counts(&self) -> Result<UserCounts, ApiError>;
    /// Sign-ups per UTC day over the last `days` days, oldest first.
    async fn signups(&self, days: i64) -> Result<Vec<DailyCount>, ApiError>;
    async fn moderation_open(&self) -> Result<i64, ApiError>;
    async fn emails_pending(&self) -> Result<i64, ApiError>;
    /// `pattern` is an escaped `ILIKE` pattern; `number` a ticket number
    /// typed as such.
    async fn search(
        &self,
        pattern: &str,
        number: Option<i64>,
    ) -> Result<ConsoleSearchResults, ApiError>;
    /// The latest sign-in attempts on `user_id` (90 days, at most
    /// `limit`).
    async fn sign_ins(&self, user_id: Uuid, limit: i64) -> Result<Vec<SignInEvent>, ApiError>;
}

pub struct ConsoleRepositoryImpl {
    pub db: PgPool,
}

impl ConsoleRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

#[async_trait::async_trait]
impl ConsoleRepository for ConsoleRepositoryImpl {
    async fn user_counts(&self) -> Result<UserCounts, ApiError> {
        let now = now();
        let today = now.date().and_hms_opt(0, 0, 0).unwrap_or(now);
        Ok(sqlx::query_as::<_, UserCounts>(
            "SELECT
                COUNT(*) AS total,
                COUNT(*) FILTER (WHERE created_at >= $2) AS new_today,
                COUNT(*) FILTER (WHERE created_at >= $1 - INTERVAL '7 days') AS new_7d,
                COUNT(*) FILTER (WHERE created_at >= $1 - INTERVAL '30 days') AS new_30d,
                COUNT(*) FILTER (WHERE last_login_at >= $1 - INTERVAL '7 days') AS active_7d,
                COUNT(*) FILTER (WHERE last_login_at >= $1 - INTERVAL '30 days') AS active_30d,
                COUNT(*) FILTER (WHERE email_verified_at IS NULL OR email IS NULL) AS unverified,
                COUNT(*) FILTER (WHERE banned_at IS NOT NULL
                                   AND (banned_until IS NULL OR banned_until > $1)) AS banned,
                COUNT(*) FILTER (WHERE status = 'inactive') AS deactivated,
                COUNT(*) FILTER (WHERE role IN ('admin', 'moderator')
                                   AND (totp_enabled_at IS NULL OR totp_secret_enc IS NULL))
                    AS staff_without_2fa
             FROM users",
        )
        .bind(now)
        .bind(today)
        .fetch_one(&self.db)
        .await?)
    }

    async fn signups(&self, days: i64) -> Result<Vec<DailyCount>, ApiError> {
        let today = now().date();
        let first = today - Duration::days(days - 1);
        // The days as `date` (a timestamptz series is estimated at 1 000
        // rows and pushed the plan past Postgres' JIT threshold, which then
        // compiled code on every overview for a few milliseconds of work).
        Ok(sqlx::query_as::<_, DailyCount>(
            "SELECT d.day, COALESCE(c.count, 0) AS count
             FROM (SELECT generate_series($1::date, $2::date, INTERVAL '1 day')::date AS day) d
             LEFT JOIN (SELECT created_at::date AS day, COUNT(*) AS count
                        FROM users WHERE created_at >= $1::date
                        GROUP BY 1) c ON c.day = d.day
             ORDER BY d.day",
        )
        .bind(first)
        .bind(today)
        .fetch_all(&self.db)
        .await?)
    }

    async fn moderation_open(&self) -> Result<i64, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM moderation_flags WHERE status = 'open'")
                .fetch_one(&self.db)
                .await?,
        )
    }

    async fn emails_pending(&self) -> Result<i64, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM email_outbox WHERE status = 'pending'")
                .fetch_one(&self.db)
                .await?,
        )
    }

    async fn search(
        &self,
        pattern: &str,
        number: Option<i64>,
    ) -> Result<ConsoleSearchResults, ApiError> {
        let users = sqlx::query_as::<_, SearchUser>(
            "SELECT id, username, email, avatar_url, role FROM users
             WHERE username ILIKE $1 OR email ILIKE $1 OR first_name ILIKE $1 OR last_name ILIKE $1
             ORDER BY LOWER(username) LIMIT $2",
        )
        .bind(pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&self.db);
        let bands = sqlx::query_as::<_, SearchBand>(
            "SELECT id, name, logo_url FROM bands WHERE name ILIKE $1
             ORDER BY LOWER(name) LIMIT $2",
        )
        .bind(pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&self.db);
        let songs = sqlx::query_as::<_, SearchContent>(
            "SELECT s.id, s.title, a.name AS subtitle, u.username AS owner_username
             FROM songs s
             JOIN artists a ON a.id = s.artist_id
             JOIN users u ON u.id = s.user_id
             WHERE s.deleted_at IS NULL AND (s.title ILIKE $1 OR a.name ILIKE $1)
             ORDER BY s.updated_at DESC LIMIT $2",
        )
        .bind(pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&self.db);
        let setlists = sqlx::query_as::<_, SearchContent>(
            "SELECT s.id, s.title, NULL::text AS subtitle, u.username AS owner_username
             FROM setlists s
             JOIN users u ON u.id = s.user_id
             WHERE s.deleted_at IS NULL AND s.title ILIKE $1
             ORDER BY s.updated_at DESC LIMIT $2",
        )
        .bind(pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&self.db);
        let tickets = sqlx::query_as::<_, SearchTicket>(
            "SELECT t.id, t.number, t.subject, t.status, t.priority, u.username
             FROM support_tickets t
             JOIN users u ON u.id = t.user_id
             WHERE t.subject ILIKE $1 OR t.number = $3
             ORDER BY t.last_message_at DESC LIMIT $2",
        )
        .bind(pattern)
        .bind(SEARCH_LIMIT)
        .bind(number)
        .fetch_all(&self.db);
        let (users, bands, songs, setlists, tickets) =
            tokio::try_join!(users, bands, songs, setlists, tickets)?;
        Ok(ConsoleSearchResults {
            users,
            bands,
            songs,
            setlists,
            tickets,
        })
    }

    async fn sign_ins(&self, user_id: Uuid, limit: i64) -> Result<Vec<SignInEvent>, ApiError> {
        Ok(sqlx::query_as::<_, SignInEvent>(
            "SELECT CASE action
                        WHEN 'user.login_succeeded' THEN 'succeeded'
                        WHEN 'user.login_locked' THEN 'locked'
                        ELSE 'failed' END AS outcome,
                    metadata->>'method' AS method,
                    COALESCE((metadata->>'second_factor')::boolean, FALSE) AS second_factor,
                    ip_address, created_at
             FROM audit_logs
             WHERE target_type = 'user' AND target_id = $1
               AND action IN ('user.login_succeeded', 'user.login_failed', 'user.login_locked')
               AND created_at >= $2
             ORDER BY created_at DESC, id DESC
             LIMIT $3",
        )
        .bind(user_id)
        .bind(now() - Duration::days(90))
        .bind(limit)
        .fetch_all(&self.db)
        .await?)
    }
}
