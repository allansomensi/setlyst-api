//! The e-mail delivery console: what the outbox holds and how delivery
//! is going. Template variables are never exposed (they can carry
//! one-time codes).

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct OutboxEmail {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    pub to_email: String,
    pub template: String,
    pub locale: String,
    /// `pending`, `sending`, `sent`, `failed` or `skipped`.
    pub status: String,
    pub priority: i16,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// Whether it can be queued again (`failed` or `skipped`, with its
    /// variables still stored).
    pub retryable: bool,
    pub scheduled_at: NaiveDateTime,
    pub sent_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OutboxQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// `pending`, `sending`, `sent`, `failed` or `skipped`.
    pub status: Option<String>,
    /// Exact template name (`password_reset_code`, `announcement`...).
    pub template: Option<String>,
    /// Recipient address or username.
    pub q: Option<String>,
    /// Only e-mails about this account.
    pub user_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct OutboxCounts {
    pub pending: i64,
    pub sending: i64,
    pub sent: i64,
    pub failed: i64,
    pub skipped: i64,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct TemplateCount {
    pub template: String,
    pub sent: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OutboxSummary {
    /// Whether an SMTP server is configured (otherwise messages are only
    /// logged and marked `skipped`).
    pub smtp_configured: bool,
    /// The sender address.
    pub from: String,
    /// Non-security e-mails sent per hour at most.
    pub hourly_cap: i64,
    /// Non-security e-mails sent in the last hour.
    pub sent_last_hour: i64,
    /// Oldest message still waiting, if any.
    pub oldest_pending_at: Option<NaiveDateTime>,
    pub last_24h: OutboxCounts,
    pub last_7d: OutboxCounts,
    /// Per template over the last 7 days, most sent first.
    pub templates: Vec<TemplateCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TestEmailResponse {
    pub id: Uuid,
    pub to: String,
}
