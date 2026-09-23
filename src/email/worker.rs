//! Delivers the e-mail outbox.
//!
//! Each run claims a batch of due messages with `FOR UPDATE SKIP LOCKED`
//! (several API instances can run the worker at once without sending
//! anything twice), renders and sends them, and records the outcome:
//!
//! - sent → `sent`;
//! - failed → retried with exponential back-off, up to [`MAX_ATTEMPTS`],
//!   then `failed`;
//! - no SMTP server configured (development) → the rendered text is logged
//!   and the message is marked `skipped`.
//!
//! Templates carrying one-time codes have their payload wiped as soon as
//! the message leaves the `pending` state for good.

use super::{
    templates::{EmailTemplate, Locale, RenderContext, RenderedEmail},
    unsubscribe,
};
use crate::{
    config::{Config, SmtpConfig, SmtpTls},
    errors::api_error::ApiError,
};
use chrono::{Duration, NaiveDateTime, Utc};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Delivery attempts before a message is given up on.
pub const MAX_ATTEMPTS: i32 = 6;
/// Messages claimed per run.
pub const BATCH_SIZE: i64 = 25;
/// A message stuck in `sending` this long (a crashed worker) is retried.
const STALE_LOCK_MINUTES: i64 = 10;

/// Something that can deliver a rendered message.
#[async_trait::async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, to: &str, email: &RenderedEmail) -> Result<(), String>;
}

/// SMTP delivery through `lettre`.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    reply_to: Option<Mailbox>,
}

impl SmtpMailer {
    pub fn from_config(config: &SmtpConfig) -> Result<Self, String> {
        let builder = match config.tls {
            SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(|e| e.to_string())?,
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)
                .map_err(|e| e.to_string())?,
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
        };
        let mut builder = builder
            .port(config.port)
            .timeout(Some(std::time::Duration::from_secs(20)));
        if let (Some(user), Some(password)) = (&config.username, &config.password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from: config
                .from
                .parse()
                .map_err(|e| format!("invalid SMTP_FROM: {e}"))?,
            reply_to: match &config.reply_to {
                Some(r) => Some(
                    r.parse()
                        .map_err(|e| format!("invalid SMTP_REPLY_TO: {e}"))?,
                ),
                None => None,
            },
        })
    }
}

#[async_trait::async_trait]
impl MailTransport for SmtpMailer {
    async fn send(&self, to: &str, email: &RenderedEmail) -> Result<(), String> {
        let to: Mailbox = to.parse().map_err(|e| format!("invalid recipient: {e}"))?;
        let mut builder = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(email.subject.clone());
        if let Some(reply_to) = &self.reply_to {
            builder = builder.reply_to(reply_to.clone());
        }
        let message = builder
            .multipart(MultiPart::alternative_plain_html(
                email.text.clone(),
                email.html.clone(),
            ))
            .map_err(|e| e.to_string())?;
        self.transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, FromRow)]
struct ClaimedEmail {
    id: Uuid,
    user_id: Option<Uuid>,
    to_email: String,
    template: String,
    locale: String,
    payload: Value,
    attempts: i32,
}

/// Delay before retry number `attempts` (1-based): 30 s, 1 min, 2 min...
pub fn backoff(attempts: i32) -> Duration {
    let exponent = (attempts - 1).clamp(0, 10) as u32;
    Duration::seconds(30 * 2i64.pow(exponent))
}

/// What happened to one claimed message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Skipped,
    Retrying,
    Failed,
}

/// Renders `template` for `user_id` in `locale` (with an unsubscribe link
/// where the template has one).
pub fn render_for(
    template: &EmailTemplate,
    user_id: Option<Uuid>,
    locale: &str,
    app_base_url: &str,
) -> RenderedEmail {
    let locale = Locale::parse(locale);
    let unsubscribe_url = match (user_id, template.unsubscribe_category()) {
        (Some(user_id), Some(category)) => Some(unsubscribe::link(
            app_base_url,
            locale.code(),
            user_id,
            category,
        )),
        _ => None,
    };
    template.render(&RenderContext {
        app_base_url: app_base_url.to_string(),
        locale,
        unsubscribe_url,
    })
}

/// Processes one batch of due messages. `transport` is `None` when no SMTP
/// server is configured. Returns the outcome of each claimed message.
pub async fn process_batch(
    pool: &PgPool,
    transport: Option<&dyn MailTransport>,
    app_base_url: &str,
) -> Result<Vec<Delivery>, ApiError> {
    let now = Utc::now().naive_utc();

    // Messages left in `sending` by a worker that died mid-delivery.
    sqlx::query(
        "UPDATE email_outbox SET status = 'pending', locked_at = NULL
         WHERE status = 'sending' AND locked_at < $1",
    )
    .bind(now - Duration::minutes(STALE_LOCK_MINUTES))
    .execute(pool)
    .await?;

    let claimed: Vec<ClaimedEmail> = sqlx::query_as(
        "UPDATE email_outbox SET status = 'sending', locked_at = $1, attempts = attempts + 1
         WHERE id IN (
             SELECT id FROM email_outbox
             WHERE status = 'pending' AND scheduled_at <= $1
             ORDER BY scheduled_at
             LIMIT $2
             FOR UPDATE SKIP LOCKED
         )
         RETURNING id, user_id, to_email, template, locale, payload, attempts",
    )
    .bind(now)
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;

    let mut outcomes = Vec::with_capacity(claimed.len());
    for email in claimed {
        let outcome = deliver(pool, transport, app_base_url, &email).await?;
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

async fn deliver(
    pool: &PgPool,
    transport: Option<&dyn MailTransport>,
    app_base_url: &str,
    email: &ClaimedEmail,
) -> Result<Delivery, ApiError> {
    let Some(template) = EmailTemplate::from_parts(&email.template, &email.payload) else {
        error!(id = %email.id, template = %email.template, "Unknown or malformed e-mail template");
        finish(pool, email.id, "failed", Some("unknown template"), true).await?;
        return Ok(Delivery::Failed);
    };
    let sensitive = template.is_sensitive();
    let rendered = render_for(&template, email.user_id, &email.locale, app_base_url);

    let Some(transport) = transport else {
        info!(
            id = %email.id,
            to = %email.to_email,
            template = %email.template,
            subject = %rendered.subject,
            "SMTP is not configured; e-mail not sent:\n{}",
            rendered.text
        );
        finish(pool, email.id, "skipped", None, sensitive).await?;
        return Ok(Delivery::Skipped);
    };

    match transport.send(&email.to_email, &rendered).await {
        Ok(()) => {
            finish(pool, email.id, "sent", None, sensitive).await?;
            Ok(Delivery::Sent)
        }
        Err(e) if email.attempts >= MAX_ATTEMPTS => {
            error!(id = %email.id, error = %e, "E-mail delivery failed for good");
            finish(pool, email.id, "failed", Some(&e), sensitive).await?;
            Ok(Delivery::Failed)
        }
        Err(e) => {
            warn!(id = %email.id, attempts = email.attempts, error = %e, "E-mail delivery failed; will retry");
            let retry_at: NaiveDateTime = Utc::now().naive_utc() + backoff(email.attempts);
            sqlx::query(
                "UPDATE email_outbox SET status = 'pending', locked_at = NULL, scheduled_at = $2,
                        last_error = $3
                 WHERE id = $1",
            )
            .bind(email.id)
            .bind(retry_at)
            .bind(e.chars().take(1000).collect::<String>())
            .execute(pool)
            .await?;
            Ok(Delivery::Retrying)
        }
    }
}

async fn finish(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    error: Option<&str>,
    clear_payload: bool,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE email_outbox
         SET status = $2::email_status, locked_at = NULL,
             sent_at = CASE WHEN $2 = 'sent' THEN $3 ELSE sent_at END,
             last_error = COALESCE($4, last_error),
             payload = CASE WHEN $5 THEN '{}'::jsonb ELSE payload END
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(Utc::now().naive_utc())
    .bind(error.map(|e| e.chars().take(1000).collect::<String>()))
    .bind(clear_payload)
    .execute(pool)
    .await?;
    Ok(())
}

/// Builds the SMTP transport from the configuration, if any.
pub fn transport_from_config(config: &Config) -> Option<Box<dyn MailTransport>> {
    let smtp = config.smtp.as_ref()?;
    match SmtpMailer::from_config(smtp) {
        Ok(mailer) => Some(Box::new(mailer)),
        Err(e) => {
            error!(error = %e, "SMTP configuration is invalid; e-mails will only be logged");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(backoff(1), Duration::seconds(30));
        assert_eq!(backoff(2), Duration::seconds(60));
        assert_eq!(backoff(5), Duration::seconds(480));
        assert_eq!(backoff(0), Duration::seconds(30));
    }
}
