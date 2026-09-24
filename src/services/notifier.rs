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
//!   announcements and release notes have their own e-mail fan-out;
//! - billing notices about a paid subscription (purchase, cancellation and
//!   withdrawal confirmations, failed payments, charge reminders) are
//!   e-mailed even when account e-mails are switched off: the law requires
//!   them (Decreto 7.962 art. 4, CDC art. 6).

use crate::{
    database::AppState,
    email::{
        EmailTemplate, OutgoingEmail, enqueue, notification_text::describe, templates::Locale,
    },
    errors::api_error::ApiError,
    models::{
        notification::{Notification, NotificationType},
        user::{CURRENT_TERMS_VERSION, Status},
    },
    services::billing::BILLING_NOTICE_KINDS,
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
    let billing = kind == NotificationType::SubscriptionChanged
        && notification.data["kind"]
            .as_str()
            .is_some_and(|k| BILLING_NOTICE_KINDS.contains(&k));

    if channel.in_app {
        state.notification_repo.create(notification).await?;
    }
    if !channel.email && !billing {
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
        NotificationType::SubscriptionChanged => {
            billing_template(state, notification, recipient.username).await?
        }
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

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

/// The paid subscription as the e-mail describes it.
#[derive(sqlx::FromRow)]
struct PaidTerms {
    unit_amount_cents: Option<i64>,
    currency: Option<String>,
    billing_interval: Option<String>,
    current_period_end: Option<NaiveDateTime>,
    trial_ends_at: Option<NaiveDateTime>,
    terms_version: Option<String>,
}

/// The e-mail of a `subscription_changed` notice, by its `kind`.
async fn billing_template(
    state: &AppState,
    notification: &Notification,
    username: String,
) -> Result<EmailTemplate, ApiError> {
    let data = &notification.data;
    let kind = data["kind"].as_str().unwrap_or("plan_granted");
    let plan_name = plan_name(state, data["plan_code"].as_str()).await?;
    Ok(match kind {
        // The contract confirmation: plan, price, period, next charge,
        // the terms accepted and how to cancel or withdraw.
        "subscribed" => {
            let terms: Option<PaidTerms> = sqlx::query_as(
                "SELECT unit_amount_cents, currency, billing_interval, current_period_end,
                        trial_ends_at, terms_version
                 FROM subscriptions WHERE user_id = $1 AND source = 'payment'",
            )
            .bind(notification.user_id)
            .fetch_optional(&state.db)
            .await?;
            match terms {
                Some(terms) => EmailTemplate::SubscriptionConfirmed {
                    username,
                    plan_name,
                    amount_cents: terms.unit_amount_cents,
                    currency: terms.currency,
                    interval: terms.billing_interval,
                    next_charge_at: terms.trial_ends_at.or(terms.current_period_end),
                    trial_ends_at: terms.trial_ends_at,
                    terms_version: Some(
                        terms
                            .terms_version
                            .unwrap_or_else(|| CURRENT_TERMS_VERSION.to_string()),
                    ),
                },
                None => EmailTemplate::SubscriptionChanged {
                    username,
                    kind: kind.to_string(),
                    plan_name,
                    current_period_end: timestamp(&data["current_period_end"]),
                },
            }
        }
        "paid_trial_ending" => EmailTemplate::PaidTrialEnding {
            username,
            plan_name,
            amount_cents: data["amount_cents"].as_i64(),
            currency: text(&data["currency"]),
            interval: text(&data["interval"]),
            charge_at: timestamp(&data["charge_at"]).unwrap_or_default(),
        },
        "renewal_reminder" => EmailTemplate::RenewalReminder {
            username,
            plan_name,
            amount_cents: data["amount_cents"].as_i64(),
            currency: text(&data["currency"]),
            renews_at: timestamp(&data["renews_at"]).unwrap_or_default(),
        },
        "withdrawn" | "refunded" => EmailTemplate::WithdrawalConfirmed {
            username,
            plan_name,
            refunded_cents: data["refunded_cents"].as_i64().unwrap_or(0),
            currency: text(&data["currency"]).unwrap_or_else(|| "brl".into()),
            requested_at: notification.created_at,
            by_staff: kind == "refunded",
        },
        "disputed" => EmailTemplate::PaymentDisputed {
            username,
            plan_name,
        },
        "price_change" => EmailTemplate::PriceChange {
            username,
            plan_name,
            interval: text(&data["interval"]),
            old_amount_cents: data["old_amount_cents"].as_i64(),
            new_amount_cents: data["new_amount_cents"].as_i64().unwrap_or(0),
            currency: text(&data["currency"]),
            effective_at: timestamp(&data["effective_at"]).unwrap_or_default(),
        },
        _ => EmailTemplate::SubscriptionChanged {
            username,
            kind: kind.to_string(),
            plan_name,
            current_period_end: timestamp(&data["current_period_end"]),
        },
    })
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
