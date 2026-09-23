//! Announcement delivery: in-app notifications and e-mails, sent once per
//! announcement when its window starts.

use crate::{
    database::AppState,
    email::{EmailTemplate, OutgoingEmail, outbox::enqueue_many},
    errors::api_error::ApiError,
};
use tracing::info;
use uuid::Uuid;

/// Fans out announcement `id` if it is due and not delivered yet.
/// Returns whether this call delivered it.
pub async fn deliver(state: &AppState, id: Uuid) -> Result<bool, ApiError> {
    let Some((announcement, fan_out)) = state.announcement_repo.fan_out(id).await? else {
        return Ok(false);
    };

    let emails: Vec<OutgoingEmail> = fan_out
        .email_recipients
        .into_iter()
        .map(|recipient| OutgoingEmail {
            user_id: Some(recipient.user_id),
            to: recipient.email,
            locale: recipient.language,
            template: EmailTemplate::Announcement {
                title: announcement.title.clone(),
                body: announcement.body.clone(),
                level: announcement.level.key().to_string(),
                cta_label: announcement.cta_label.clone(),
                cta_url: announcement.cta_url.clone(),
            },
        })
        .collect();
    let queued = enqueue_many(&state.db, &emails).await?;

    info!(
        announcement_id = %id,
        notifications = fan_out.notifications,
        emails = queued,
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
