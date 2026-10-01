use crate::{
    errors::api_error::ApiError,
    models::email_admin::{OutboxCounts, OutboxEmail, OutboxQuery, TemplateCount},
};
use chrono::{Duration, NaiveDateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Templates whose variables are wiped once they leave the queue (one-time
/// codes): they can never be sent again.
const CODE_TEMPLATES: [&str; 4] = [
    "email_verification_code",
    "password_reset_code",
    "email_change_code",
    "reauth_code",
];

macro_rules! outbox_columns {
    () => {
        "o.id, o.user_id, u.username, o.to_email, o.template, o.locale, o.status::text AS status,
         o.priority, o.attempts, o.last_error,
         (o.status IN ('failed', 'skipped') AND o.payload <> '{}'::jsonb
          AND NOT (o.template = ANY($1))) AS retryable,
         o.scheduled_at, o.sent_at, o.created_at
         FROM email_outbox o
         LEFT JOIN users u ON u.id = o.user_id"
    };
}

/// `$2` status, `$3` template, `$4` search pattern, `$5` account.
macro_rules! outbox_filter {
    () => {
        " WHERE ($2::text IS NULL OR o.status::text = $2)
            AND ($3::text IS NULL OR o.template = $3)
            AND ($4::text IS NULL OR o.to_email ILIKE $4 OR u.username ILIKE $4)
            AND ($5::uuid IS NULL OR o.user_id = $5)"
    };
}

#[derive(FromRow)]
struct StatusCount {
    status: String,
    count: i64,
}

#[async_trait::async_trait]
pub trait EmailAdminRepository: Send + Sync {
    async fn list(
        &self,
        query: &OutboxQuery,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<OutboxEmail>, i64), ApiError>;
    async fn find(&self, id: Uuid) -> Result<Option<OutboxEmail>, ApiError>;
    /// Messages per status created since `since`.
    async fn counts_since(&self, since: NaiveDateTime) -> Result<OutboxCounts, ApiError>;
    async fn templates_since(&self, since: NaiveDateTime) -> Result<Vec<TemplateCount>, ApiError>;
    async fn sent_last_hour(&self) -> Result<i64, ApiError>;
    async fn oldest_pending(&self) -> Result<Option<NaiveDateTime>, ApiError>;
    /// Queues a failed or skipped message again. `false` when it isn't
    /// retryable.
    async fn retry(&self, id: Uuid) -> Result<bool, ApiError>;
    /// Cancels a message still waiting (marks it `skipped`). `false` when
    /// it isn't pending.
    async fn cancel(&self, id: Uuid) -> Result<bool, ApiError>;
    /// Failed messages in the last 24 hours (the console's overview).
    async fn failed_last_day(&self) -> Result<i64, ApiError>;
}

pub struct EmailAdminRepositoryImpl {
    pub db: PgPool,
}

impl EmailAdminRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

fn code_templates() -> Vec<String> {
    CODE_TEMPLATES.iter().map(|t| t.to_string()).collect()
}

#[async_trait::async_trait]
impl EmailAdminRepository for EmailAdminRepositoryImpl {
    async fn list(
        &self,
        query: &OutboxQuery,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<OutboxEmail>, i64), ApiError> {
        let status = query.status.as_deref().filter(|s| !s.is_empty());
        let template = query.template.as_deref().filter(|t| !t.is_empty());
        let total = sqlx::query_scalar(
            "SELECT COUNT(*) FROM email_outbox o LEFT JOIN users u ON u.id = o.user_id
             WHERE ($1::text IS NULL OR o.status::text = $1)
               AND ($2::text IS NULL OR o.template = $2)
               AND ($3::text IS NULL OR o.to_email ILIKE $3 OR u.username ILIKE $3)
               AND ($4::uuid IS NULL OR o.user_id = $4)",
        )
        .bind(status)
        .bind(template)
        .bind(search)
        .bind(query.user_id)
        .fetch_one(&self.db);
        let rows = sqlx::query_as::<_, OutboxEmail>(concat!(
            "SELECT ",
            outbox_columns!(),
            outbox_filter!(),
            " ORDER BY o.created_at DESC, o.id DESC LIMIT $6 OFFSET $7"
        ))
        .bind(code_templates())
        .bind(status)
        .bind(template)
        .bind(search)
        .bind(query.user_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);
        let (total, rows) = tokio::try_join!(total, rows)?;
        Ok((rows, total))
    }

    async fn find(&self, id: Uuid) -> Result<Option<OutboxEmail>, ApiError> {
        Ok(sqlx::query_as::<_, OutboxEmail>(concat!(
            "SELECT ",
            outbox_columns!(),
            " WHERE o.id = $2"
        ))
        .bind(code_templates())
        .bind(id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn counts_since(&self, since: NaiveDateTime) -> Result<OutboxCounts, ApiError> {
        let rows = sqlx::query_as::<_, StatusCount>(
            "SELECT status::text AS status, COUNT(*) AS count FROM email_outbox
             WHERE created_at >= $1 GROUP BY status",
        )
        .bind(since)
        .fetch_all(&self.db)
        .await?;
        let mut counts = OutboxCounts::default();
        for row in rows {
            match row.status.as_str() {
                "pending" => counts.pending = row.count,
                "sending" => counts.sending = row.count,
                "sent" => counts.sent = row.count,
                "failed" => counts.failed = row.count,
                "skipped" => counts.skipped = row.count,
                _ => {}
            }
        }
        Ok(counts)
    }

    async fn templates_since(&self, since: NaiveDateTime) -> Result<Vec<TemplateCount>, ApiError> {
        Ok(sqlx::query_as::<_, TemplateCount>(
            "SELECT template,
                    COUNT(*) FILTER (WHERE status = 'sent') AS sent,
                    COUNT(*) FILTER (WHERE status = 'failed') AS failed
             FROM email_outbox WHERE created_at >= $1
             GROUP BY template ORDER BY COUNT(*) DESC, template LIMIT 40",
        )
        .bind(since)
        .fetch_all(&self.db)
        .await?)
    }

    async fn sent_last_hour(&self) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM email_outbox
             WHERE status = 'sent' AND priority > 0 AND sent_at >= $1",
        )
        .bind(now() - Duration::hours(1))
        .fetch_one(&self.db)
        .await?)
    }

    async fn oldest_pending(&self) -> Result<Option<NaiveDateTime>, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT MIN(created_at) FROM email_outbox WHERE status = 'pending'")
                .fetch_one(&self.db)
                .await?,
        )
    }

    async fn retry(&self, id: Uuid) -> Result<bool, ApiError> {
        let updated = sqlx::query(
            "UPDATE email_outbox
             SET status = 'pending', attempts = 0, scheduled_at = $2, locked_at = NULL,
                 last_error = NULL
             WHERE id = $1 AND status IN ('failed', 'skipped') AND payload <> '{}'::jsonb
               AND NOT (template = ANY($3))",
        )
        .bind(id)
        .bind(now())
        .bind(code_templates())
        .execute(&self.db)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn cancel(&self, id: Uuid) -> Result<bool, ApiError> {
        let updated = sqlx::query(
            "UPDATE email_outbox
             SET status = 'skipped', last_error = 'Canceled by staff',
                 payload = CASE WHEN template = ANY($2) THEN '{}'::jsonb ELSE payload END
             WHERE id = $1 AND status = 'pending'",
        )
        .bind(id)
        .bind(code_templates())
        .execute(&self.db)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn failed_last_day(&self) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM email_outbox WHERE status = 'failed' AND created_at >= $1",
        )
        .bind(now() - Duration::hours(24))
        .fetch_one(&self.db)
        .await?)
    }
}
