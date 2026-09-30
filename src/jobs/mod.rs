//! Background jobs, started by `server::run` (never by the integration
//! tests, which call the job bodies directly when they need them).
//!
//! Each job runs in its own Tokio task on a fixed interval. A failing run
//! is logged and the job simply tries again at the next tick; a run that
//! panics or overstays [`MAX_RUN`] ends, not the schedule. The first runs
//! are staggered so a fresh instance doesn't open every job's statements
//! at once on a pool it is about to serve traffic from, and every loop
//! stops scheduling new runs at shutdown (see [`crate::utils::tasks`]).
//!
//! Retention work goes in batches of [`RETENTION_BATCH`] rows: each batch
//! is its own short statement, so a backlog is worked off progressively
//! instead of one huge DELETE that hits the statement timeout, rolls back
//! and never catches up.

pub mod accounts;
pub mod announcements;
pub mod trash;

use crate::{
    config::Config,
    database::AppState,
    email::worker::{self, BATCH_SIZE},
    errors::api_error::ApiError,
    services::billing,
    utils::tasks,
};
use chrono::{Duration, NaiveDateTime, Utc};
use rand::RngExt;
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

/// Longest a single run may take. Past it the run is abandoned (its
/// statement is cancelled with its connection) and logged, and the job
/// waits for its next tick, so one wedged run (an SMTP server that
/// stopped answering, a lock never granted) can't freeze a job for good.
pub const MAX_RUN: StdDuration = StdDuration::from_secs(15 * 60);

/// Rows one retention statement touches at a time.
pub const RETENTION_BATCH: i64 = 5_000;

/// Up to this much is added to a job's first run, so the replicas of a
/// deployment don't all run the same job at the same moment.
const FIRST_RUN_JITTER_SECS: u64 = 30;

/// Runs `job` every `every`, forever, the first time `first_after` (plus a
/// little jitter) from now, logging failures. Each run is its own task: a
/// panic ends that run, not the schedule.
pub(crate) fn every<F, Fut>(
    name: &'static str,
    every: StdDuration,
    first_after: StdDuration,
    job: F,
) where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<u64, ApiError>> + Send + 'static,
{
    let jitter = StdDuration::from_secs(rand::rng().random_range(0..=FIRST_RUN_JITTER_SECS));
    tasks::spawn(async move {
        let shutdown = tasks::shutdown_token();
        let start = tokio::time::Instant::now() + first_after + jitter;
        let mut interval = tokio::time::interval_at(start, every);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.cancelled() => break,
            }
            let run = tokio::spawn(job());
            let abort = run.abort_handle();
            match tokio::time::timeout(MAX_RUN, run).await {
                Ok(Ok(Ok(0))) => debug!(job = name, "Job run: nothing to do"),
                Ok(Ok(Ok(count))) => info!(job = name, count, "Job run"),
                Ok(Ok(Err(e))) => error!(job = name, error = %e, "Job run failed"),
                Ok(Err(join)) if join.is_panic() => error!(job = name, "Job run panicked"),
                Ok(Err(_)) => {}
                Err(_) => {
                    abort.abort();
                    error!(
                        job = name,
                        max_secs = MAX_RUN.as_secs(),
                        "Job run took too long and was abandoned"
                    );
                }
            }
        }
        debug!(job = name, "Job stopped");
    });
}

/// Runs `statement` — whose `WHERE id IN (SELECT id ... LIMIT $n)` bounds
/// it to [`RETENTION_BATCH`] rows, `$n` being the parameter right after
/// `cutoffs` — until a batch comes back short. Each batch is its own
/// transaction. Returns the rows touched in total.
async fn in_batches(
    pool: &PgPool,
    statement: &'static str,
    cutoffs: &[NaiveDateTime],
) -> Result<u64, ApiError> {
    let mut total = 0u64;
    loop {
        let mut query = sqlx::query(statement);
        for cutoff in cutoffs {
            query = query.bind(*cutoff);
        }
        let touched = query
            .bind(RETENTION_BATCH)
            .execute(pool)
            .await?
            .rows_affected();
        total += touched;
        if touched < RETENTION_BATCH as u64 || tasks::is_shutting_down() {
            return Ok(total);
        }
        // A breather between batches, for other writers and autovacuum.
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }
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
            StdDuration::from_secs(5),
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
            StdDuration::from_secs(2 * 60),
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
            StdDuration::from_secs(3 * 60),
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
            StdDuration::from_secs(45),
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
            StdDuration::from_secs(20),
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
            StdDuration::from_secs(5 * 60),
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
            StdDuration::from_secs(7 * 60),
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
        every(
            "trash_purge",
            StdDuration::from_secs(3600),
            StdDuration::from_secs(4 * 60),
            move || {
                let pool = pool.clone();
                async move { trash::purge_trash(&pool, retention_days).await }
            },
        );
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
    in_batches(
        pool,
        "DELETE FROM email_outbox WHERE id IN (
             SELECT id FROM email_outbox
             WHERE status IN ('sent', 'skipped', 'failed') AND created_at < $1
             LIMIT $2)",
        &[cutoff],
    )
    .await
}

/// Deletes what would otherwise pile up forever: read notifications older
/// than [`READ_NOTIFICATION_RETENTION_DAYS`] and any older than
/// [`NOTIFICATION_RETENTION_DAYS`]; band invites revoked or expired more
/// than [`INACTIVE_INVITE_RETENTION_DAYS`] ago; suggestions closed more
/// than [`CLOSED_SUGGESTION_RETENTION_DAYS`] ago (their votes cascade).
/// Returns the number of rows deleted.
pub async fn apply_content_retention(pool: &PgPool) -> Result<u64, ApiError> {
    let now = Utc::now().naive_utc();
    let notifications = in_batches(
        pool,
        "DELETE FROM notifications WHERE id IN (
             SELECT id FROM notifications
             WHERE (read_at IS NOT NULL AND created_at < $1) OR created_at < $2
             LIMIT $3)",
        &[
            now - Duration::days(READ_NOTIFICATION_RETENTION_DAYS),
            now - Duration::days(NOTIFICATION_RETENTION_DAYS),
        ],
    )
    .await?;
    let invites = in_batches(
        pool,
        "DELETE FROM band_invites WHERE id IN (
             SELECT id FROM band_invites
             WHERE COALESCE(revoked_at, expires_at) < $1
             LIMIT $2)",
        &[now - Duration::days(INACTIVE_INVITE_RETENTION_DAYS)],
    )
    .await?;
    let suggestions = in_batches(
        pool,
        "DELETE FROM band_song_suggestions WHERE id IN (
             SELECT id FROM band_song_suggestions
             WHERE status <> 'open' AND COALESCE(resolved_at, updated_at) < $1
             LIMIT $2)",
        &[now - Duration::days(CLOSED_SUGGESTION_RETENTION_DAYS)],
    )
    .await?;
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
    // The rows still carrying access data are found through the partial
    // index `idx_audit_logs_access_pending` (migration 0021), so the
    // steady-state run only visits what the last one left.
    let anonymized = in_batches(
        pool,
        "UPDATE audit_logs
         SET ip_address = NULL,
             target_label = CASE WHEN action = 'user.login_failed' AND target_id IS NULL
                                 THEN NULL ELSE target_label END
         WHERE id IN (
             SELECT id FROM audit_logs
             WHERE created_at < $1
               AND (ip_address IS NOT NULL
                    OR (action = 'user.login_failed' AND target_id IS NULL AND target_label IS NOT NULL))
               -- The sign-up address stays while the account exists: the
               -- referral program compares it to spot self-referrals
               -- (services/billing.rs, qualify_referral; Privacy Policy).
               AND NOT (action = 'user.registered'
                        AND EXISTS (SELECT 1 FROM users u WHERE u.id = audit_logs.target_id))
             LIMIT $2)",
        &[access_cutoff],
    )
    .await?;
    let deleted = in_batches(
        pool,
        "DELETE FROM audit_logs WHERE id IN (
             SELECT id FROM audit_logs WHERE created_at < $1 LIMIT $2)",
        &[now - Duration::days(AUDIT_RETENTION_DAYS)],
    )
    .await?;
    Ok(anonymized + deleted)
}
