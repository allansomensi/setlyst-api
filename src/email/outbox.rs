//! The e-mail outbox.
//!
//! Messages are written to `email_outbox` (ideally in the same transaction
//! as the change that caused them) and delivered by the background worker
//! (`email::worker`), so a slow or unavailable SMTP server never blocks or
//! fails a request.

use super::templates::EmailTemplate;
use crate::{
    errors::api_error::ApiError,
    models::{communication::Category, user::canonical_email},
};
use chrono::{NaiveDateTime, Utc};
use sqlx::{Executor, Postgres};
use tracing::warn;
use uuid::Uuid;

/// One message to enqueue.
#[derive(Debug, Clone)]
pub struct OutgoingEmail {
    /// The account the message is about (`None` once it no longer exists,
    /// e.g. the goodbye message of a deleted account).
    pub user_id: Option<Uuid>,
    pub to: String,
    pub locale: String,
    pub template: EmailTemplate,
}

/// Delivery priority of queued messages (`email_outbox.priority`); lower
/// is sent first, so a bulk send never delays a code that expires in
/// minutes.
pub mod priority {
    /// One-time codes and security notices.
    pub const SECURITY: i16 = 0;
    /// Account and billing messages.
    pub const ACCOUNT: i16 = 3;
    /// Everything else (copies of in-app notifications). Also the column
    /// default.
    pub const NORMAL: i16 = 5;
    /// Announcements and release notes, sent to many users at once.
    pub const BULK: i16 = 9;
}

/// One-time-code e-mails to the same mailbox (per template) accepted in
/// 24 hours for codes that can go to an *arbitrary* address (sign-up
/// verification, e-mail change), whatever account asked for them: nothing
/// else stops them from being used to flood someone's inbox. Further
/// requests are dropped (see [`enqueue`]).
pub const CODE_EMAILS_PER_ADDRESS_PER_DAY: i64 = 5;
/// Codes that only ever go to the account's own registered address
/// (password recovery, re-authentication): the issuer's per-account
/// allowance is what bounds them, so this cap only has to stay above it
/// (below it, a stranger's requests would starve the owner of codes).
pub const ACCOUNT_CODE_EMAILS_PER_ADDRESS_PER_DAY: i64 = 20;
/// Copies of in-app notifications to one mailbox per day.
pub const NOTIFICATION_EMAILS_PER_ADDRESS_PER_DAY: i64 = 30;
/// Everything else (security notices, account and billing messages) to
/// one mailbox, per template, per day. Nobody legitimately gets more than
/// a handful of "your password was changed" messages in a day; a script
/// looping a settings change on an account with someone else's
/// (unverified) address would otherwise send thousands.
pub const OTHER_EMAILS_PER_ADDRESS_PER_DAY: i64 = 10;
/// Non-code messages queued for one account per day, all templates
/// together: a stolen or throw-away session looping a notified action
/// (linking and unlinking Google, toggling a band mate's role...) can't
/// use up the provider's quota or the hourly cap.
pub const EMAILS_PER_ACCOUNT_PER_DAY: i64 = 50;

/// How many messages of `template` one mailbox may be sent in 24 hours,
/// or `None` for bulk mail (which admins send deliberately, through
/// [`enqueue_many`]).
pub fn recipient_daily_cap(template: &str) -> Option<i64> {
    match template {
        "announcement" | "release_notes" => None,
        "password_reset_code" | "reauth_code" => Some(ACCOUNT_CODE_EMAILS_PER_ADDRESS_PER_DAY),
        name if is_code_template(name) => Some(CODE_EMAILS_PER_ADDRESS_PER_DAY),
        "notification" => Some(NOTIFICATION_EMAILS_PER_ADDRESS_PER_DAY),
        _ => Some(OTHER_EMAILS_PER_ADDRESS_PER_DAY),
    }
}

/// The outbox priority of `template`. Unknown templates (added later) get
/// a priority from their name: `*_code` are codes, billing words mean an
/// account message, anything else is [`priority::NORMAL`].
pub fn priority_of(template: &EmailTemplate) -> i16 {
    match template {
        EmailTemplate::Notification {
            category: Category::Security,
            ..
        } => return priority::SECURITY,
        EmailTemplate::Notification { .. } => return priority::NORMAL,
        _ => {}
    }
    priority_of_name(template.name())
}

fn priority_of_name(name: &str) -> i16 {
    match name {
        "password_changed"
        | "two_factor_enabled"
        | "two_factor_disabled"
        | "email_changed_notice"
        | "account_deleted"
        | "security_notice" => priority::SECURITY,
        "announcement" | "release_notes" => priority::BULK,
        "welcome" => priority::ACCOUNT,
        name if is_code_template(name) => priority::SECURITY,
        name if [
            "trial",
            "subscription",
            "payment",
            "billing",
            "invoice",
            "refund",
            "withdraw",
        ]
        .iter()
        .any(|word| name.contains(word)) =>
        {
            priority::ACCOUNT
        }
        _ => priority::NORMAL,
    }
}

/// Templates carrying a one-time code, capped per recipient.
pub fn is_code_template(name: &str) -> bool {
    name.ends_with("_code")
}

/// Enqueues one message. `executor` may be the pool or an open
/// transaction.
///
/// Every template is capped per recipient mailbox (see
/// [`recipient_daily_cap`]; the mailbox is the canonical form of the
/// address, so `+tags` and Gmail dots don't multiply it), and non-code
/// messages are also capped per account ([`EMAILS_PER_ACCOUNT_PER_DAY`]).
/// Past a cap the message is silently dropped (logged, never reported to
/// the caller, so the answer of a sign-up or recovery request doesn't
/// reveal anything) and the nil id is returned.
pub async fn enqueue<'e, E>(executor: E, email: &OutgoingEmail) -> Result<Uuid, ApiError>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let now = Utc::now().naive_utc();
    let (template, payload) = email.template.to_parts();
    let to: String = email.to.trim().chars().take(254).collect();
    let canonical = canonical_email(&to);
    let recipient_cap = recipient_daily_cap(template);
    let account_cap = (!is_code_template(template) && email.user_id.is_some())
        .then_some(EMAILS_PER_ACCOUNT_PER_DAY);
    // One statement (the executor may be a transaction, usable once): the
    // row is only written while the recipient and the account are under
    // their caps.
    let inserted = sqlx::query(
        "INSERT INTO email_outbox (id, user_id, to_email, to_canonical, template, locale, payload,
                                   status, attempts, scheduled_at, created_at, priority)
         SELECT $1, $2, $3, $10, $4, $5, $6, 'pending', 0, $7, $7, $8
         WHERE ($9::bigint IS NULL
                OR (SELECT COUNT(*) FROM email_outbox
                    WHERE to_canonical = $10 AND template = $4
                      AND created_at > $7 - INTERVAL '24 hours') < $9)
           AND ($11::bigint IS NULL
                OR (SELECT COUNT(*) FROM email_outbox
                    WHERE user_id = $2 AND template NOT LIKE '%\\_code' ESCAPE '\\'
                      AND created_at > $7 - INTERVAL '24 hours') < $11)",
    )
    .bind(id)
    .bind(email.user_id)
    .bind(&to)
    .bind(template)
    .bind(&email.locale)
    .bind(payload)
    .bind(now)
    .bind(priority_of(&email.template))
    .bind(recipient_cap)
    .bind(&canonical)
    .bind(account_cap)
    .execute(executor)
    .await?
    .rows_affected();
    if inserted == 0 {
        warn!(
            template,
            to = %mask_email(&to),
            user_id = ?email.user_id,
            "E-mail cap reached (per recipient or per account); message dropped"
        );
        return Ok(Uuid::nil());
    }
    Ok(id)
}

/// Enqueues many messages with one statement per chunk of 500.
pub async fn enqueue_many(pool: &sqlx::PgPool, emails: &[OutgoingEmail]) -> Result<u64, ApiError> {
    if emails.is_empty() {
        return Ok(0);
    }
    let mut conn = pool.acquire().await?;
    enqueue_many_in(&mut conn, emails).await
}

/// [`enqueue_many`] on a given connection, e.g. inside the transaction of
/// the change that sends them (committed or rolled back together).
pub async fn enqueue_many_in(
    conn: &mut sqlx::PgConnection,
    emails: &[OutgoingEmail],
) -> Result<u64, ApiError> {
    let mut inserted = 0;
    for chunk in emails.chunks(500) {
        let now: NaiveDateTime = Utc::now().naive_utc();
        let mut ids = Vec::with_capacity(chunk.len());
        let mut users = Vec::with_capacity(chunk.len());
        let mut tos = Vec::with_capacity(chunk.len());
        let mut canonicals = Vec::with_capacity(chunk.len());
        let mut templates = Vec::with_capacity(chunk.len());
        let mut locales = Vec::with_capacity(chunk.len());
        let mut payloads = Vec::with_capacity(chunk.len());
        let mut priorities = Vec::with_capacity(chunk.len());
        for email in chunk {
            let (template, payload) = email.template.to_parts();
            let to: String = email.to.trim().chars().take(254).collect();
            ids.push(Uuid::now_v7());
            users.push(email.user_id);
            canonicals.push(canonical_email(&to));
            tos.push(to);
            templates.push(template.to_string());
            locales.push(email.locale.clone());
            payloads.push(payload);
            priorities.push(priority_of(&email.template));
        }
        let result = sqlx::query(
            "INSERT INTO email_outbox (id, user_id, to_email, to_canonical, template, locale, payload,
                                       status, attempts, scheduled_at, created_at, priority)
             SELECT t.id, t.user_id, t.to_email, t.to_canonical, t.template, t.locale, t.payload,
                    'pending', 0, $7, $7, t.priority
             FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $9::text[], $4::text[], $5::text[],
                         $6::jsonb[], $8::smallint[])
                  AS t(id, user_id, to_email, to_canonical, template, locale, payload, priority)",
        )
        .bind(&ids)
        .bind(&users)
        .bind(&tos)
        .bind(&templates)
        .bind(&locales)
        .bind(&payloads)
        .bind(now)
        .bind(&priorities)
        .bind(&canonicals)
        .execute(&mut *conn)
        .await?;
        inserted += result.rows_affected();
    }
    Ok(inserted)
}

/// Masks an address for display in notices sent elsewhere:
/// `ana.maria@example.com` → `a***@example.com`.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        None => "***".to_string(),
    }
}

/// Masks every e-mail address inside free text (SMTP error messages often
/// quote the recipient), word by word, with [`mask_email`].
pub fn mask_emails_in(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            // Keep the punctuation around an address (`<a@b.c>:`) visible.
            let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
            if trimmed.contains('@') && !trimmed.starts_with('@') && !trimmed.ends_with('@') {
                out.push_str(&word.replace(trimmed, &mask_email(trimmed)));
            } else {
                out.push_str(word);
            }
            word.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>' | '(' | ')' | '"' | '\'') {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_addresses() {
        assert_eq!(mask_email("ana.maria@example.com"), "a***@example.com");
        assert_eq!(mask_email("x"), "***");
    }

    #[test]
    fn masks_addresses_inside_smtp_errors() {
        assert_eq!(
            mask_emails_in("550 5.1.1 <ana.maria@example.com>: Recipient address rejected"),
            "550 5.1.1 <a***@example.com>: Recipient address rejected"
        );
        assert_eq!(
            mask_emails_in("mailbox bob@x.org, full; retry"),
            "mailbox b***@x.org, full; retry"
        );
        assert_eq!(mask_emails_in("421 try again later"), "421 try again later");
    }

    #[test]
    fn priorities_put_codes_first_and_bulk_last() {
        assert_eq!(priority_of_name("password_reset_code"), priority::SECURITY);
        assert_eq!(priority_of_name("reauth_code"), priority::SECURITY);
        assert_eq!(priority_of_name("two_factor_disabled"), priority::SECURITY);
        assert_eq!(priority_of_name("subscription_changed"), priority::ACCOUNT);
        assert_eq!(priority_of_name("withdrawal_confirmed"), priority::ACCOUNT);
        assert_eq!(priority_of_name("welcome"), priority::ACCOUNT);
        assert_eq!(priority_of_name("announcement"), priority::BULK);
        assert_eq!(priority_of_name("release_notes"), priority::BULK);
        assert_eq!(priority_of_name("something_new"), priority::NORMAL);
        assert_eq!(priority_of_name("security_notice"), priority::SECURITY);
        assert!(is_code_template("email_verification_code"));
        assert!(!is_code_template("welcome"));
    }

    #[test]
    fn every_template_but_bulk_mail_is_capped_per_mailbox() {
        assert_eq!(recipient_daily_cap("announcement"), None);
        assert_eq!(recipient_daily_cap("release_notes"), None);
        assert_eq!(
            recipient_daily_cap("email_verification_code"),
            Some(CODE_EMAILS_PER_ADDRESS_PER_DAY)
        );
        assert_eq!(
            recipient_daily_cap("password_reset_code"),
            Some(ACCOUNT_CODE_EMAILS_PER_ADDRESS_PER_DAY)
        );
        assert_eq!(
            recipient_daily_cap("notification"),
            Some(NOTIFICATION_EMAILS_PER_ADDRESS_PER_DAY)
        );
        for name in [
            "password_changed",
            "security_notice",
            "welcome",
            "subscription_changed",
        ] {
            assert_eq!(
                recipient_daily_cap(name),
                Some(OTHER_EMAILS_PER_ADDRESS_PER_DAY),
                "{name}"
            );
        }
        // The recovery cap must never starve the owner: the code issuer
        // allows fewer per account than the mailbox accepts.
        const {
            assert!(
                ACCOUNT_CODE_EMAILS_PER_ADDRESS_PER_DAY
                    >= crate::services::account::CODE_DAILY_LIMIT
            );
        }
    }
}
