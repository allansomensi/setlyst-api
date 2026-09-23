//! The single entry point for delivering a [`Notification`].
//!
//! Every notification in the API goes through [`notify`], so delivery
//! rules live in one place instead of at each call site:
//!
//! - the notification's category (see `NotificationType::category`) and
//!   the recipient's communication preferences decide whether it is shown
//!   in the app and whether an e-mail copy is sent;
//! - e-mail copies only go to verified addresses of active accounts;
//! - security notifications are always shown (their e-mails are sent by
//!   the flow that triggered them, with a dedicated template), and
//!   announcements and release notes have their own e-mail fan-out.

use crate::{
    database::AppState,
    email::{
        EmailTemplate, OutgoingEmail, enqueue, notification_text::describe, templates::Locale,
    },
    errors::api_error::ApiError,
    models::{
        notification::{Notification, NotificationType},
        user::Status,
    },
};
use chrono::NaiveDateTime;
use serde_json::{Value, json};
use tracing::error;

/// Delivers `notification` to its recipient. Best effort: a failure is
/// logged and never fails the action that triggered it.
pub async fn notify(state: &AppState, notification: Notification) {
    if let Err(e) = deliver(state, &notification).await {
        error!(error = %e, user_id = %notification.user_id, kind = notification.notification_type.key(), "Failed to deliver notification");
    }
}

/// [`notify`] for several notifications.
pub async fn notify_all(state: &AppState, notifications: Vec<Notification>) {
    for notification in notifications {
        notify(state, notification).await;
    }
}

#[derive(sqlx::FromRow)]
struct Recipient {
    username: String,
    email: Option<String>,
    email_verified: bool,
    status: Status,
}

async fn deliver(state: &AppState, notification: &Notification) -> Result<(), ApiError> {
    let kind = notification.notification_type;
    let category = kind.category();
    let (prefs, language) = state
        .user_prefs_repo
        .get_communication(notification.user_id)
        .await?;
    let channel = prefs.get(category);

    if channel.in_app {
        state.notification_repo.create(notification).await?;
    }
    if !channel.email {
        return Ok(());
    }

    let Some(recipient) = sqlx::query_as::<_, Recipient>(
        "SELECT username, email, (email_verified_at IS NOT NULL) AS email_verified, status
         FROM users WHERE id = $1",
    )
    .bind(notification.user_id)
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    let Some(email) = recipient.email.filter(|_| recipient.email_verified) else {
        return Ok(());
    };
    if recipient.status != Status::Active {
        return Ok(());
    }

    let locale = language.unwrap_or_else(|| "en".to_string());
    let data = &notification.data;
    let template = match kind {
        // Delivered by e-mail through their own paths.
        NotificationType::Announcement
        | NotificationType::ReleasePublished
        | NotificationType::SecurityAlert => return Ok(()),
        NotificationType::SubscriptionChanged => EmailTemplate::SubscriptionChanged {
            username: recipient.username,
            kind: data["kind"].as_str().unwrap_or("plan_granted").to_string(),
            plan_name: plan_name(state, data["plan_code"].as_str()).await?,
            current_period_end: timestamp(&data["current_period_end"]),
        },
        NotificationType::TrialEnding => {
            let Some(ends_at) = timestamp(&data["ends_at"]) else {
                return Ok(());
            };
            EmailTemplate::TrialEnding {
                username: recipient.username,
                plan_name: plan_name(state, data["plan_code"].as_str()).await?,
                ends_at,
            }
        }
        other => {
            let text = describe(other, data, Locale::parse(&locale));
            EmailTemplate::Notification {
                title: text.title,
                lines: text.lines,
                cta_label: None,
                cta_path: text.cta_path,
                category,
            }
        }
    };

    enqueue(
        &state.db,
        &OutgoingEmail {
            user_id: Some(notification.user_id),
            to: email,
            locale,
            template,
        },
    )
    .await?;
    Ok(())
}

fn timestamp(value: &Value) -> Option<NaiveDateTime> {
    serde_json::from_value(value.clone()).ok()
}

/// The localized name map of a plan (the code itself when unknown).
async fn plan_name(state: &AppState, code: Option<&str>) -> Result<Value, ApiError> {
    let Some(code) = code else {
        return Ok(json!({}));
    };
    let name: Option<Value> = sqlx::query_scalar("SELECT name FROM plans WHERE code = $1")
        .bind(code)
        .fetch_optional(&state.db)
        .await?;
    Ok(name.unwrap_or_else(|| json!({ "en": code })))
}
