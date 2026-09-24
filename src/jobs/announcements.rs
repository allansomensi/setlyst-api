//! Announcement delivery: in-app notifications and e-mails, sent once per
//! announcement when its window starts.

use crate::{database::AppState, errors::api_error::ApiError};
use tracing::info;
use uuid::Uuid;

/// Fans out announcement `id` if it is due and not delivered yet.
/// Returns whether this call delivered it.
pub async fn deliver(state: &AppState, id: Uuid) -> Result<bool, ApiError> {
    // The claim, the notifications and the e-mails are one transaction.
    let Some((_, fan_out)) = state.announcement_repo.fan_out(id).await? else {
        return Ok(false);
    };

    info!(
        announcement_id = %id,
        notifications = fan_out.notifications,
        emails = fan_out.emails,
        "Announcement delivered"
    );
    Ok(true)
}

/// Delivers every announcement whose window has started. Returns how many
/// were delivered.
pub async fn deliver_due(state: &AppState) -> Result<u64, ApiError> {
    let mut delivered = 0;
    for id in state.announcement_repo.due_for_delivery().await? {
        if deliver(state, id).await? {
            delivered += 1;
        }
    }
    Ok(delivered)
}
