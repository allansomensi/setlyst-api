//! The e-mail outbox.
//!
//! Messages are written to `email_outbox` (ideally in the same transaction
//! as the change that caused them) and delivered by the background worker
//! (`email::worker`), so a slow or unavailable SMTP server never blocks or
//! fails a request.

use super::templates::EmailTemplate;
use crate::errors::api_error::ApiError;
use chrono::{NaiveDateTime, Utc};
use sqlx::{Executor, Postgres};
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

/// Enqueues one message. `executor` may be the pool or an open
/// transaction.
pub async fn enqueue<'e, E>(executor: E, email: &OutgoingEmail) -> Result<Uuid, ApiError>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let now = Utc::now().naive_utc();
    let (template, payload) = email.template.to_parts();
    let to: String = email.to.trim().chars().take(254).collect();
    sqlx::query(
        "INSERT INTO email_outbox (id, user_id, to_email, template, locale, payload, status,
                                   attempts, scheduled_at, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', 0, $7, $7)",
    )
    .bind(id)
    .bind(email.user_id)
    .bind(to)
    .bind(template)
    .bind(&email.locale)
    .bind(payload)
    .bind(now)
    .execute(executor)
    .await?;
    Ok(id)
}

/// Enqueues many messages with one statement per chunk of 500.
pub async fn enqueue_many(pool: &sqlx::PgPool, emails: &[OutgoingEmail]) -> Result<u64, ApiError> {
    let mut inserted = 0;
    for chunk in emails.chunks(500) {
        let now: NaiveDateTime = Utc::now().naive_utc();
        let mut ids = Vec::with_capacity(chunk.len());
        let mut users = Vec::with_capacity(chunk.len());
        let mut tos = Vec::with_capacity(chunk.len());
        let mut templates = Vec::with_capacity(chunk.len());
        let mut locales = Vec::with_capacity(chunk.len());
        let mut payloads = Vec::with_capacity(chunk.len());
        for email in chunk {
            let (template, payload) = email.template.to_parts();
            ids.push(Uuid::now_v7());
            users.push(email.user_id);
            tos.push(email.to.trim().chars().take(254).collect::<String>());
            templates.push(template.to_string());
            locales.push(email.locale.clone());
            payloads.push(payload);
        }
        let result = sqlx::query(
            "INSERT INTO email_outbox (id, user_id, to_email, template, locale, payload, status,
                                       attempts, scheduled_at, created_at)
             SELECT t.id, t.user_id, t.to_email, t.template, t.locale, t.payload, 'pending', 0, $7, $7
             FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $4::text[], $5::text[], $6::jsonb[])
                  AS t(id, user_id, to_email, template, locale, payload)",
        )
        .bind(&ids)
        .bind(&users)
        .bind(&tos)
        .bind(&templates)
        .bind(&locales)
        .bind(&payloads)
        .bind(now)
        .execute(pool)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_addresses() {
        assert_eq!(mask_email("ana.maria@example.com"), "a***@example.com");
        assert_eq!(mask_email("x"), "***");
    }
}
