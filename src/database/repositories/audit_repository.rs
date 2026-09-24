use crate::{
    errors::api_error::ApiError,
    models::{
        audit::{AuditLogEntry, AuditLogQuery, LegalAcceptance},
        auth::access::AccessControl,
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

/// A pending audit log entry, built fluently at the call site:
///
/// ```ignore
/// AuditEvent::by(&access, actions::USER_BANNED)
///     .target("user", target.id, &target.username)
///     .meta(json!({ "until": until }))
///     .ip(&ip)
///     .record(&*state.audit_repo)
///     .await;
/// ```
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub actor_id: Option<Uuid>,
    pub actor_username: Option<String>,
    pub impersonator_id: Option<Uuid>,
    pub action: &'static str,
    pub target_type: Option<&'static str>,
    pub target_id: Option<Uuid>,
    pub target_label: Option<String>,
    pub metadata: Value,
    pub ip_address: Option<String>,
}

impl AuditEvent {
    pub fn new(action: &'static str) -> Self {
        Self {
            actor_id: None,
            actor_username: None,
            impersonator_id: None,
            action,
            target_type: None,
            target_id: None,
            target_label: None,
            metadata: json!({}),
            ip_address: None,
        }
    }

    /// An event performed by the authenticated caller.
    pub fn by(access: &AccessControl, action: &'static str) -> Self {
        let mut event = Self::new(action);
        event.actor_id = Some(access.user_id());
        event.actor_username = Some(access.0.username.clone());
        event.impersonator_id = access.impersonator();
        event
    }

    pub fn actor(mut self, id: Uuid, username: &str) -> Self {
        self.actor_id = Some(id);
        self.actor_username = Some(username.to_string());
        self
    }

    pub fn target(mut self, kind: &'static str, id: Uuid, label: &str) -> Self {
        self.target_type = Some(kind);
        self.target_id = Some(id);
        self.target_label = Some(label.chars().take(255).collect());
        self
    }

    /// A target known only by a label (e.g. the identifier typed in a
    /// failed sign-in for an account that doesn't exist).
    pub fn target_label(mut self, kind: &'static str, label: &str) -> Self {
        self.target_type = Some(kind);
        self.target_id = None;
        self.target_label = Some(label.chars().take(64).collect());
        self
    }

    pub fn meta(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn ip(mut self, ip: &Option<String>) -> Self {
        self.ip_address = ip.clone();
        self
    }

    /// Records the entry in the background, off the request's hot path
    /// (used where the time taken must not depend on the outcome, such as
    /// failed sign-ins).
    pub fn spawn(self, repo: std::sync::Arc<dyn AuditRepository>) {
        tokio::spawn(async move {
            self.record(&*repo).await;
        });
    }

    /// Persists the entry. Never fails the surrounding request: an audit
    /// write error is logged loudly instead, since refusing a legitimate
    /// action because the log table hiccupped would be worse.
    pub async fn record(self, repo: &dyn AuditRepository) {
        let action = self.action;
        if let Err(e) = repo.record(self).await {
            error!(action, error = %e, "Failed to write audit log entry");
        }
    }
}

#[async_trait::async_trait]
pub trait AuditRepository: Send + Sync {
    async fn record(&self, event: AuditEvent) -> Result<(), ApiError>;
    async fn list(
        &self,
        query: &AuditLogQuery,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<AuditLogEntry>, i64), ApiError>;
    /// Records consents given or withdrawn (the legal acceptance ledger).
    async fn record_legal_acceptances(&self, rows: &[LegalAcceptance]) -> Result<(), ApiError>;
}

/// Records `rows` in the legal acceptance ledger without failing the
/// request (like audit entries: a ledger hiccup is logged loudly).
pub async fn record_legal_acceptances(repo: &dyn AuditRepository, rows: &[LegalAcceptance]) {
    if let Err(e) = repo.record_legal_acceptances(rows).await {
        error!(error = %e, "Failed to record legal acceptances");
    }
}

pub struct AuditRepositoryImpl {
    pub db: PgPool,
}

impl AuditRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl AuditRepository for AuditRepositoryImpl {
    async fn record(&self, event: AuditEvent) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_username, impersonator_id, action, target_type,
                                     target_id, target_label, metadata, ip_address, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::now_v7())
        .bind(event.actor_id)
        .bind(event.actor_username)
        .bind(event.impersonator_id)
        .bind(event.action)
        .bind(event.target_type)
        .bind(event.target_id)
        .bind(event.target_label)
        .bind(event.metadata)
        .bind(event.ip_address)
        .bind(chrono::Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn list(
        &self,
        query: &AuditLogQuery,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<AuditLogEntry>, i64), ApiError> {
        let offset = (page - 1) * per_page;

        // Action filter: "user.banned" matches exactly, "user." matches the
        // whole family.
        let action_pattern = query.action.as_deref().map(|action| {
            let escaped = action
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            if action.ends_with('.') {
                format!("{escaped}%")
            } else {
                escaped
            }
        });
        let search = query
            .q
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| {
                format!(
                    "%{}%",
                    q.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                )
            });

        // The WHERE clause is repeated verbatim in both queries: sqlx only
        // accepts literal query strings. Keep them in sync.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs a
             WHERE ($1::uuid IS NULL OR a.actor_id = $1)
               AND ($2::uuid IS NULL OR a.target_id = $2)
               AND ($3::text IS NULL OR a.action LIKE $3)
               AND ($4::text IS NULL OR a.actor_username ILIKE $4 OR a.target_label ILIKE $4)",
        )
        .bind(query.actor_id)
        .bind(query.target_id)
        .bind(&action_pattern)
        .bind(&search)
        .fetch_one(&self.db)
        .await?;

        let entries = sqlx::query_as::<_, AuditLogEntry>(
            "SELECT a.id, a.actor_id, a.actor_username, a.impersonator_id, a.action, a.target_type,
                    a.target_id, a.target_label, a.metadata, a.ip_address, a.created_at
             FROM audit_logs a
             WHERE ($1::uuid IS NULL OR a.actor_id = $1)
               AND ($2::uuid IS NULL OR a.target_id = $2)
               AND ($3::text IS NULL OR a.action LIKE $3)
               AND ($4::text IS NULL OR a.actor_username ILIKE $4 OR a.target_label ILIKE $4)
             ORDER BY a.created_at DESC
             LIMIT $5 OFFSET $6",
        )
        .bind(query.actor_id)
        .bind(query.target_id)
        .bind(&action_pattern)
        .bind(&search)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&self.db)
        .await?;

        Ok((entries, count))
    }

    async fn record_legal_acceptances(&self, rows: &[LegalAcceptance]) -> Result<(), ApiError> {
        if rows.is_empty() {
            return Ok(());
        }
        let timestamp = chrono::Utc::now().naive_utc();
        let mut tx = self.db.begin().await?;
        for row in rows {
            sqlx::query(
                "INSERT INTO legal_acceptances (id, user_id, document, version, accepted, source,
                                                ip_address, user_agent, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(Uuid::now_v7())
            .bind(row.user_id)
            .bind(row.document)
            .bind(&row.version)
            .bind(row.accepted)
            .bind(row.source)
            .bind(&row.ip_address)
            .bind(
                row.user_agent
                    .as_deref()
                    .map(|ua| ua.chars().take(255).collect::<String>()),
            )
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
