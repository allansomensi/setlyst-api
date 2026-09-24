//! Delivers the e-mail outbox.
//!
//! Each run claims a batch of due messages with `FOR UPDATE SKIP LOCKED`
//! (several API instances can run the worker at once without sending
//! anything twice) — highest priority first (`outbox::priority`: codes and
//! security notices before everything else, bulk mail last) — renders and
//! sends them, a few at a time over pooled SMTP connections, and records
//! the outcome:
//!
//! - sent → `sent`;
//! - failed → retried with exponential back-off, up to [`MAX_ATTEMPTS`],
//!   then `failed`;
//! - no SMTP server configured (development) → the rendered text is logged
//!   and the message is marked `skipped`;
//! - the recipient switched that category of e-mail off after the message
//!   was queued → `skipped`.
//!
//! Past `Config::email_hourly_cap` non-security messages sent in the last
//! hour, only priority-0 messages (codes, security notices) are claimed;
//! the rest wait for the next hour (an error is logged once an hour).
//!
//! Templates carrying one-time codes have their payload wiped as soon as
//! the message leaves the `pending` state for good. Error texts are stored
//! and logged with every e-mail address in them masked.

use super::{
    outbox::{mask_emails_in, priority},
    templates::{EmailTemplate, Locale, RenderContext, RenderedEmail},
    unsubscribe,
};
use crate::{
    config::{Config, SmtpConfig, SmtpTls},
    errors::api_error::ApiError,
    models::communication::CommunicationPreferences,
};
use chrono::{Duration, NaiveDateTime, Utc};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{
        Mailbox, MultiPart,
        header::{HeaderName, HeaderValue},
    },
    transport::smtp::authentication::Credentials,
};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use std::sync::atomic::{AtomicI64, Ordering};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Delivery attempts before a message is given up on.
pub const MAX_ATTEMPTS: i32 = 6;
/// Messages claimed per run.
pub const BATCH_SIZE: i64 = 25;
/// Messages of one batch in flight at once (each on its own pooled SMTP
/// connection).
pub const SEND_CONCURRENCY: usize = 4;
/// Non-security messages per hour when no configuration is loaded.
pub const DEFAULT_HOURLY_CAP: i64 = 500;
/// A message stuck in `sending` this long (a crashed worker) is retried.
const STALE_LOCK_MINUTES: i64 = 10;

/// Something that can deliver a rendered message.
#[async_trait::async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, to: &str, email: &RenderedEmail) -> Result<(), String>;
}

/// SMTP delivery through `lettre`, over a small pool of reused
/// connections (no TCP + TLS + AUTH handshake per message).
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    reply_to: Option<Mailbox>,
    /// Public API origin for the RFC 8058 one-click unsubscribe link.
    api_public_url: Option<String>,
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
            .timeout(Some(std::time::Duration::from_secs(20)))
            .pool_config(
                lettre::transport::smtp::PoolConfig::new()
                    .max_size(SEND_CONCURRENCY as u32)
                    .idle_timeout(std::time::Duration::from_secs(60)),
            );
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
            api_public_url: Config::try_get().and_then(|c| c.api_public_url.clone()),
        })
    }
}

/// The `List-Unsubscribe` header value for a message whose footer links
/// to `web_url` (`.../unsubscribe?token=...`): the one-click API endpoint
/// first when the API's public origin is known, then the web page. The
/// second element says whether one-click (RFC 8058) is offered.
pub fn list_unsubscribe(web_url: &str, api_public_url: Option<&str>) -> (String, bool) {
    let token = web_url
        .split_once("token=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or_default())
        .filter(|token| !token.is_empty());
    match (api_public_url, token) {
        (Some(api), Some(token)) => (
            format!(
                "<{}/api/v1/public/email/unsubscribe/one-click?token={token}>, <{web_url}>",
                api.trim_end_matches('/')
            ),
            true,
        ),
        _ => (format!("<{web_url}>"), false),
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
        // Mail providers show their own "unsubscribe" button for these
        // (and require them from bulk senders).
        if let Some(web_url) = &email.unsubscribe_url {
            let (value, one_click) = list_unsubscribe(web_url, self.api_public_url.as_deref());
            builder = builder.raw_header(HeaderValue::new(
                HeaderName::new_from_ascii_str("List-Unsubscribe"),
                value,
            ));
            if one_click {
                builder = builder.raw_header(HeaderValue::new(
                    HeaderName::new_from_ascii_str("List-Unsubscribe-Post"),
                    "List-Unsubscribe=One-Click".to_string(),
                ));
            }
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
    priority: i16,
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

/// Non-security messages sent in the last hour (what the hourly cap is
/// compared with).
async fn sent_last_hour(pool: &PgPool, now: NaiveDateTime) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_outbox
         WHERE status = 'sent' AND priority > $2 AND sent_at > $1",
    )
    .bind(now - Duration::hours(1))
    .bind(priority::SECURITY)
    .fetch_one(pool)
    .await?)
}

/// The hour (as a Unix hour number) the cap was last reported in, so a
/// saturated queue logs one error per hour rather than one per run.
static CAP_REPORTED_HOUR: AtomicI64 = AtomicI64::new(-1);

/// Processes one batch of due messages. `transport` is `None` when no SMTP
/// server is configured. Returns the outcome of each claimed message, in
/// claim order.
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

    // Past the hourly cap only codes and security notices go out (a burst
    // of notifications or a sign-up flood must not burn the provider's
    // quota that password resets depend on).
    let cap = Config::try_get().map_or(DEFAULT_HOURLY_CAP, |c| c.email_hourly_cap);
    let sent = sent_last_hour(pool, now).await?;
    let max_priority = if sent >= cap {
        let hour = now.and_utc().timestamp() / 3600;
        if CAP_REPORTED_HOUR.swap(hour, Ordering::Relaxed) != hour {
            error!(
                sent,
                cap,
                "Hourly e-mail cap reached: only codes and security notices are sent until it frees up"
            );
        }
        priority::SECURITY
    } else {
        i16::MAX
    };

    let claimed: Vec<ClaimedEmail> = sqlx::query_as(
        "UPDATE email_outbox SET status = 'sending', locked_at = $1, attempts = attempts + 1
         WHERE id IN (
             SELECT id FROM email_outbox
             WHERE status = 'pending' AND scheduled_at <= $1 AND priority <= $3
             ORDER BY priority, scheduled_at
             LIMIT $2
             FOR UPDATE SKIP LOCKED
         )
         RETURNING id, user_id, to_email, template, locale, payload, attempts, priority",
    )
    .bind(now)
    .bind(BATCH_SIZE)
    .bind(max_priority)
    .fetch_all(pool)
    .await?;
    let mut claimed = claimed;
    claimed.sort_by_key(|email| (email.priority, email.id));

    // Boxed so the stream's future type doesn't carry a higher-ranked
    // borrow (which would make the job's future not provably `Send`).
    let deliveries: Vec<BoxFuture<'_, Result<Delivery, ApiError>>> = claimed
        .iter()
        .map(|email| deliver(pool, transport, app_base_url, email).boxed())
        .collect();
    let outcomes: Vec<Result<Delivery, ApiError>> = stream::iter(deliveries)
        .buffered(SEND_CONCURRENCY)
        .collect()
        .await;
    outcomes.into_iter().collect()
}

/// Whether the recipient still wants this kind of e-mail: a message that
/// can be unsubscribed from is checked again at send time, so switching a
/// category off also stops what was queued before (a large announcement
/// can take a while to go out).
async fn still_wanted(
    pool: &PgPool,
    template: &EmailTemplate,
    user_id: Option<Uuid>,
) -> Result<bool, ApiError> {
    let (Some(user_id), Some(category)) = (user_id, template.unsubscribe_category()) else {
        return Ok(true);
    };
    let stored: Option<Value> =
        sqlx::query_scalar("SELECT communication FROM user_preferences WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let prefs = stored
        .map(|value| CommunicationPreferences::from_stored(&value))
        .unwrap_or_default();
    Ok(prefs.get(category).email)
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

    if !still_wanted(pool, &template, email.user_id).await? {
        info!(id = %email.id, template = %email.template, "Recipient opted out after the e-mail was queued; skipped");
        finish(
            pool,
            email.id,
            "skipped",
            Some("recipient opted out"),
            sensitive,
        )
        .await?;
        return Ok(Delivery::Skipped);
    }

    let rendered = render_for(&template, email.user_id, &email.locale, app_base_url);

    let Some(transport) = transport else {
        // The body (which may hold a sign-in or reset code) is only ever
        // printed by development builds, to make local testing possible.
        if cfg!(debug_assertions) {
            info!(
                id = %email.id,
                to = %email.to_email,
                template = %email.template,
                subject = %rendered.subject,
                "SMTP is not configured; e-mail not sent:\n{}",
                rendered.text
            );
        } else {
            warn!(
                id = %email.id,
                template = %email.template,
                "SMTP is not configured; e-mail not sent"
            );
        }
        finish(pool, email.id, "skipped", None, sensitive).await?;
        return Ok(Delivery::Skipped);
    };

    match transport.send(&email.to_email, &rendered).await {
        Ok(()) => {
            finish(pool, email.id, "sent", None, sensitive).await?;
            Ok(Delivery::Sent)
        }
        Err(e) if email.attempts >= MAX_ATTEMPTS => {
            let e = mask_emails_in(&e);
            error!(id = %email.id, error = %e, "E-mail delivery failed for good");
            finish(pool, email.id, "failed", Some(&e), sensitive).await?;
            Ok(Delivery::Failed)
        }
        Err(e) => {
            let e = mask_emails_in(&e);
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

/// Checks the SMTP configuration without connecting: `Ok(false)` when there
/// is none, `Err` when it can't be used.
pub fn check_smtp_config(config: &Config) -> Result<bool, String> {
    match config.smtp.as_ref() {
        None => Ok(false),
        Some(smtp) => SmtpMailer::from_config(smtp).map(|_| true),
    }
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
    fn list_unsubscribe_offers_one_click_when_the_api_origin_is_known() {
        let web = "https://setlyst.app/pt/unsubscribe?token=abc_DEF-1";
        let (value, one_click) = list_unsubscribe(web, Some("https://api.setlyst.app/"));
        assert!(one_click);
        assert_eq!(
            value,
            "<https://api.setlyst.app/api/v1/public/email/unsubscribe/one-click?token=abc_DEF-1>, \
             <https://setlyst.app/pt/unsubscribe?token=abc_DEF-1>"
        );
        let (value, one_click) = list_unsubscribe(web, None);
        assert!(!one_click);
        assert_eq!(value, format!("<{web}>"));
    }

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(backoff(1), Duration::seconds(30));
        assert_eq!(backoff(2), Duration::seconds(60));
        assert_eq!(backoff(5), Duration::seconds(480));
        assert_eq!(backoff(0), Duration::seconds(30));
    }
}
