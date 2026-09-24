//! Account housekeeping: unverified accounts without content are removed
//! after [`UNVERIFIED_RETENTION_DAYS`], which frees the addresses they
//! claimed without proving (sign-up squatting) and keeps junk sign-ups
//! from piling up.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::audit::actions,
};
use chrono::{Duration, Utc};
use std::time::Duration as StdDuration;
use tracing::info;

/// Days an account may stay without verifying its e-mail (and without
/// any content) before it is deleted.
pub const UNVERIFIED_RETENTION_DAYS: i64 = 7;

/// Accounts deleted per batch (a run keeps going while batches are full).
const BATCH: i64 = 200;

/// Deletes unverified accounts older than [`UNVERIFIED_RETENTION_DAYS`]
/// that have no song, setlist, gig or band membership (see
/// `UserRepository::purge_unverified`). Returns how many were deleted.
pub async fn purge_unverified_accounts(state: &AppState) -> Result<u64, ApiError> {
    let cutoff = Utc::now().naive_utc() - Duration::days(UNVERIFIED_RETENTION_DAYS);
    let mut total = 0u64;
    // Bounded, so one run can't monopolize the pool.
    for _ in 0..25 {
        let purged = state.user_repo.purge_unverified(cutoff, BATCH).await?;
        for account in &purged {
            // The account is gone: only its name is kept, like other
            // deletions.
            AuditEvent::new(actions::USER_UNVERIFIED_PURGED)
                .target("user", account.id, &account.username)
                .meta(serde_json::json!({ "retention_days": UNVERIFIED_RETENTION_DAYS }))
                .record(&*state.audit_repo)
                .await;
        }
        total += purged.len() as u64;
        if (purged.len() as i64) < BATCH {
            break;
        }
    }
    if total > 0 {
        info!(count = total, "Unverified accounts purged");
    }
    Ok(total)
}

/// Starts the hourly purge (called from `jobs::spawn_all`).
pub fn spawn(state: AppState) {
    super::every(
        "unverified_account_purge",
        StdDuration::from_secs(3600),
        move || {
            let state = state.clone();
            async move { purge_unverified_accounts(&state).await }
        },
    );
}
