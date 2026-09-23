//! Background jobs, started by `server::run` (never by the integration
//! tests, which call the job bodies directly when they need them).
//!
//! Each job runs in its own Tokio task on a fixed interval. A failing run
//! is logged and the job simply tries again at the next tick: a job never
//! panics and never takes the server down.

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

/// Delivered (or skipped) e-mails are kept this long for support, then
/// deleted.
pub const OUTBOX_RETENTION_DAYS: i64 = 30;

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

/// Deletes delivered or skipped e-mails older than
/// [`OUTBOX_RETENTION_DAYS`].
pub async fn cleanup_outbox(pool: &PgPool) -> Result<u64, ApiError> {
    let cutoff = Utc::now().naive_utc() - Duration::days(OUTBOX_RETENTION_DAYS);
    Ok(sqlx::query(
        "DELETE FROM email_outbox WHERE status IN ('sent', 'skipped') AND created_at < $1",
    )
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected())
}
