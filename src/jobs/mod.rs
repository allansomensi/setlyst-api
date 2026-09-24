//! Background jobs, started by `server::run` (never by the integration
//! tests, which call the job bodies directly when they need them).
//!
//! Each job runs in its own Tokio task on a fixed interval. A failing run
//! is logged and the job simply tries again at the next tick: a job never
//! panics and never takes the server down.

pub mod accounts;
pub mod announcements;
pub mod trash;

use crate::{
    config::Config,
    database::AppState,
    email::worker::{self, BATCH_SIZE},
    errors::api_error::ApiError,
    services::billing,
};
use chrono::{Duration, Utc};
use sqlx::PgPool;
use std::{future::Future, time::Duration as StdDuration};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info};

/// Delivered, skipped or failed e-mails are kept this long for support,
/// then deleted (they hold the recipient's address and, for failures, the
/// provider's error).
pub const OUTBOX_RETENTION_DAYS: i64 = 30;

/// Read notifications are deleted after this many days...
pub const READ_NOTIFICATION_RETENTION_DAYS: i64 = 180;
/// ...and any notification after this many.
pub const NOTIFICATION_RETENTION_DAYS: i64 = 365;

/// Revoked or expired band invites are kept this long (the band's invite
/// history), then deleted.
pub const INACTIVE_INVITE_RETENTION_DAYS: i64 = 30;

/// Closed (accepted, rejected, withdrawn) song suggestions are kept this
/// long, then deleted with their votes.
pub const CLOSED_SUGGESTION_RETENTION_DAYS: i64 = 180;

/// Access records (IP address, typed sign-in identifiers) are kept for the
/// six months the Marco Civil da Internet (art. 15) requires, then
/// stripped from the audit log.
pub const ACCESS_RECORD_RETENTION_DAYS: i64 = 183;

/// Audit entries themselves are kept up to five years (Privacy Policy,
/// "Por quanto tempo guardamos"), then deleted.
pub const AUDIT_RETENTION_DAYS: i64 = 5 * 365 + 1;

/// Runs `job` every `every`, forever, logging failures.
fn every<F, Fut>(name: &'static str, every: StdDuration, job: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<u64, ApiError>> + Send,
{
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match job().await {
                Ok(0) => debug!(job = name, "Job run: nothing to do"),
                Ok(count) => info!(job = name, count, "Job run"),
                Err(e) => error!(job = name, error = %e, "Job run failed"),
            }
        }
    });
}

/// Starts every background job.
pub fn spawn_all(state: AppState) {
    let config = Config::get();

    // E-mail delivery.
    {
        let state = state.clone();
        let transport: Option<std::sync::Arc<dyn worker::MailTransport>> =
            worker::transport_from_config(config).map(std::sync::Arc::from);
        let base_url = config.app_base_url.clone();
        every(
            "email_worker",
            StdDuration::from_secs(config.email_worker_interval_secs),
            move || {
                let pool = state.db.clone();
                let transport = transport.clone();
                let base_url = base_url.clone();
                async move { run_email_worker(&pool, transport.as_deref(), &base_url).await }
            },
        );
    }

    // Expired one-time codes and sign-in challenges.
    {
        let state = state.clone();
        every(
            "purge_expired_codes",
            StdDuration::from_secs(24 * 3600),
            move || {
                let state = state.clone();
                async move { purge_expired_codes(&state).await }
            },
        );
    }

    // Old delivered e-mails.
    {
        let pool = state.db.clone();
        every(
            "outbox_cleanup",
            StdDuration::from_secs(24 * 3600),
            move || {
                let pool = pool.clone();
                async move { cleanup_outbox(&pool).await }
            },
        );
    }

    // Subscription expiry and trial reminders (no-op unless enforced).
    {
        let state = state.clone();
        every(
            "subscription_maintenance",
            StdDuration::from_secs(3600),
            move || {
                let state = state.clone();
                async move {
                    let (expired, reminded) = billing::run_subscription_maintenance(&state).await?;
                    Ok(expired + reminded)
                }
            },
        );
    }

    // Scheduled announcements.
    {
        let state = state.clone();
        every(
            "announcement_delivery",
            StdDuration::from_secs(60),
            move || {
                let state = state.clone();
                async move { announcements::deliver_due(&state).await }
            },
        );
    }

    // Audit log retention (every 6 hours).
    {
        let pool = state.db.clone();
        every(
            "audit_retention",
            StdDuration::from_secs(6 * 3600),
            move || {
                let pool = pool.clone();
                async move { apply_audit_retention(&pool).await }
            },
        );
    }

    // Content retention: notifications, old invites and suggestions
    // (daily).
    {
        let pool = state.db.clone();
        every(
            "content_retention",
            StdDuration::from_secs(24 * 3600),
            move || {
                let pool = pool.clone();
                async move { apply_content_retention(&pool).await }
            },
        );
    }

    // Trash purge (hourly).
    {
        let pool = state.db.clone();
        let retention_days = config.trash_retention_days;
        every("trash_purge", StdDuration::from_secs(3600), move || {
            let pool = pool.clone();
            async move { trash::purge_trash(&pool, retention_days).await }
        });
    }

    info!("Background jobs started");
    accounts::spawn(state);
}

/// Delivers due e-mails, batch after batch until the queue is drained.
pub async fn run_email_worker(
    pool: &PgPool,
    transport: Option<&dyn worker::MailTransport>,
    app_base_url: &str,
) -> Result<u64, ApiError> {
    let mut total = 0u64;
    // Bounded, so one run can't monopolize the task forever.
    for _ in 0..20 {
        let outcomes = worker::process_batch(pool, transport, app_base_url).await?;
        total += outcomes.len() as u64;
        if (outcomes.len() as i64) < BATCH_SIZE {
            break;
        }
    }
    Ok(total)
}

pub async fn purge_expired_codes(state: &AppState) -> Result<u64, ApiError> {
    state.security_repo.purge_expired().await
}

/// Deletes delivered, skipped or failed e-mails older than
/// [`OUTBOX_RETENTION_DAYS`].
pub async fn cleanup_outbox(pool: &PgPool) -> Result<u64, ApiError> {
    let cutoff = Utc::now().naive_utc() - Duration::days(OUTBOX_RETENTION_DAYS);
    Ok(sqlx::query(
        "DELETE FROM email_outbox
         WHERE status IN ('sent', 'skipped', 'failed') AND created_at < $1",
    )
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Deletes what would otherwise pile up forever: read notifications older
/// than [`READ_NOTIFICATION_RETENTION_DAYS`] and any older than
/// [`NOTIFICATION_RETENTION_DAYS`]; band invites revoked or expired more
/// than [`INACTIVE_INVITE_RETENTION_DAYS`] ago; suggestions closed more
/// than [`CLOSED_SUGGESTION_RETENTION_DAYS`] ago (their votes cascade).
/// Returns the number of rows deleted.
pub async fn apply_content_retention(pool: &PgPool) -> Result<u64, ApiError> {
    let now = Utc::now().naive_utc();
    let notifications = sqlx::query(
        "DELETE FROM notifications
         WHERE (read_at IS NOT NULL AND created_at < $1) OR created_at < $2",
    )
    .bind(now - Duration::days(READ_NOTIFICATION_RETENTION_DAYS))
    .bind(now - Duration::days(NOTIFICATION_RETENTION_DAYS))
    .execute(pool)
    .await?
    .rows_affected();
    let invites =
        sqlx::query("DELETE FROM band_invites WHERE COALESCE(revoked_at, expires_at) < $1")
            .bind(now - Duration::days(INACTIVE_INVITE_RETENTION_DAYS))
            .execute(pool)
            .await?
            .rows_affected();
    let suggestions = sqlx::query(
        "DELETE FROM band_song_suggestions
         WHERE status <> 'open' AND COALESCE(resolved_at, updated_at) < $1",
    )
    .bind(now - Duration::days(CLOSED_SUGGESTION_RETENTION_DAYS))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(notifications + invites + suggestions)
}

/// Applies the audit log retention: access data older than
/// [`ACCESS_RECORD_RETENTION_DAYS`] is removed (IP addresses, and the
/// identifier typed in a failed sign-in for an unknown account, which may
/// be someone's e-mail) — except the sign-up address of an account that
/// still exists — and entries older than [`AUDIT_RETENTION_DAYS`] are
/// deleted. Returns the number of rows touched.
pub async fn apply_audit_retention(pool: &PgPool) -> Result<u64, ApiError> {
    let now = Utc::now().naive_utc();
    let access_cutoff = now - Duration::days(ACCESS_RECORD_RETENTION_DAYS);
    let anonymized = sqlx::query(
        "UPDATE audit_logs
         SET ip_address = NULL,
             target_label = CASE WHEN action = 'user.login_failed' AND target_id IS NULL
                                 THEN NULL ELSE target_label END
         WHERE created_at < $1
           AND (ip_address IS NOT NULL
                OR (action = 'user.login_failed' AND target_id IS NULL AND target_label IS NOT NULL))
           -- The sign-up address stays while the account exists: the
           -- referral program compares it to spot self-referrals
           -- (services/billing.rs, qualify_referral; Privacy Policy).
           AND NOT (action = 'user.registered'
                    AND EXISTS (SELECT 1 FROM users u WHERE u.id = audit_logs.target_id))",
    )
    .bind(access_cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    let deleted = sqlx::query("DELETE FROM audit_logs WHERE created_at < $1")
        .bind(now - Duration::days(AUDIT_RETENTION_DAYS))
        .execute(pool)
        .await?
        .rows_affected();
    Ok(anonymized + deleted)
}
