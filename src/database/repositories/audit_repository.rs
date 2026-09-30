use crate::{
    errors::api_error::ApiError,
    models::{
        audit::{AuditLogEntry, AuditLogQuery, LegalAcceptance},
        auth::access::AccessControl,
    },
};
use chrono::NaiveDateTime;
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

/// How long a repeated passive staff read (the same page, the same "view
/// as" request) is folded into the entry already recorded for it.
pub const STAFF_VIEW_DEDUPE_SECONDS: i32 = 600;

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
    /// When the event happened: taken when it is built, not when a
    /// background task gets round to writing it, so spawned entries keep
    /// their place among the actions around them.
    pub created_at: NaiveDateTime,
    /// See [`AuditEvent::once_within`].
    pub dedupe_seconds: Option<i32>,
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
            created_at: chrono::Utc::now().naive_utc(),
            dedupe_seconds: None,
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

    /// Skips the entry when an identical one (same actor, impersonator,
    /// action, target and metadata) was recorded in the last `seconds`.
    /// For passive reads (staff opening a page, a "view as" session
    /// polling), so reloading a page or a background poll doesn't flood
    /// the log with copies of the same access.
    pub fn once_within(mut self, seconds: i32) -> Self {
        self.dedupe_seconds = Some(seconds);
        self
    }

    /// What [`once_within`](Self::once_within) folds copies by: the
    /// action, the actor, the impersonator, the target and the metadata.
    fn dedupe_key(&self) -> String {
        format!(
            "audit:{}:{:?}:{:?}:{:?}:{:?}:{}",
            self.action,
            self.actor_id,
            self.impersonator_id,
            self.target_type,
            self.target_id,
            self.metadata
        )
    }

    /// Records the entry in the background, off the request's hot path
    /// (used where the time taken must not depend on the outcome, such as
    /// failed sign-ins).
    pub fn spawn(self, repo: std::sync::Arc<dyn AuditRepository>) {
        crate::utils::tasks::spawn(async move {
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

/// A page row together with the total of matching entries.
#[derive(sqlx::FromRow)]
struct CountedEntry {
    total: i64,
    #[sqlx(flatten)]
    entry: AuditLogEntry,
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
        // A deduplicated entry is written under a transaction-scoped
        // advisory lock on its identity: the check below is a `NOT EXISTS`
        // in the same statement, and two copies of the same read written
        // at once (three spawned writes of a reloaded page, each waiting
        // for a pooled connection, then landing together) would each see
        // no earlier row and all go in. The lock serialises them, and the
        // second one's statement runs after the first committed, so it
        // sees the row and skips.
        let mut tx = self.db.begin().await?;
        if event.dedupe_seconds.is_some() {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(event.dedupe_key())
                .execute(&mut *tx)
                .await?;
        }
        // The actor's name is looked up when the caller only knew the id
        // (e.g. the staff member behind a "view as" session), so the entry
        // never reads as done by "the system".
        sqlx::query(
            "INSERT INTO audit_logs (id, actor_id, actor_username, impersonator_id, action, target_type,
                                     target_id, target_label, metadata, ip_address, created_at)
             SELECT $1, $2, COALESCE($3, (SELECT username FROM users WHERE id = $2)), $4, $5, $6,
                    $7, $8, $9, $10, $11
             WHERE $12::int IS NULL OR NOT EXISTS (
                 SELECT 1 FROM audit_logs d
                 WHERE d.action = $5
                   AND d.actor_id IS NOT DISTINCT FROM $2
                   AND d.impersonator_id IS NOT DISTINCT FROM $4
                   AND d.target_type IS NOT DISTINCT FROM $6
                   AND d.target_id IS NOT DISTINCT FROM $7
                   AND d.metadata = $9
                   AND d.created_at > $11 - make_interval(secs => $12::int))",
        )
        .bind(Uuid::now_v7())
        .bind(event.actor_id)
        .bind(event.actor_username.filter(|name| !name.is_empty()))
        .bind(event.impersonator_id)
        .bind(event.action)
        .bind(event.target_type)
        .bind(event.target_id)
        .bind(event.target_label)
        .bind(event.metadata)
        .bind(event.ip_address)
        .bind(event.created_at)
        .bind(event.dedupe_seconds)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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

        // The total comes with the rows (`COUNT(*) OVER ()`), so both are
        // read from the same snapshot. Entries logged in the same instant
        // are ordered by their id (UUIDv7, time-ordered), so a page always
        // holds the same rows in the same order.
        let rows = sqlx::query_as::<_, CountedEntry>(
            "SELECT COUNT(*) OVER () AS total,
                    a.id, a.actor_id, a.actor_username, a.impersonator_id, a.action, a.target_type,
                    a.target_id, a.target_label, a.metadata, a.ip_address, a.created_at
             FROM audit_logs a
             WHERE ($1::uuid IS NULL OR a.actor_id = $1)
               AND ($2::uuid IS NULL OR a.target_id = $2)
               AND ($3::text IS NULL OR a.action LIKE $3)
               AND ($4::text IS NULL OR a.actor_username ILIKE $4 OR a.target_label ILIKE $4)
               AND ($7::timestamp IS NULL OR a.created_at >= $7)
               AND ($8::timestamp IS NULL OR a.created_at < $8)
             ORDER BY a.created_at DESC, a.id DESC
             LIMIT $5 OFFSET $6",
        )
        .bind(query.actor_id)
        .bind(query.target_id)
        .bind(&action_pattern)
        .bind(&search)
        .bind(per_page)
        .bind(offset)
        .bind(query.from)
        .bind(query.to)
        .fetch_all(&self.db)
        .await?;

        // Past the last page there is no row to carry the total: count it
        // separately (the WHERE clause is repeated verbatim, sqlx only
        // accepts literal query strings; keep them in sync).
        let count = match rows.first() {
            Some(row) => row.total,
            None if offset > 0 => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM audit_logs a
                     WHERE ($1::uuid IS NULL OR a.actor_id = $1)
                       AND ($2::uuid IS NULL OR a.target_id = $2)
                       AND ($3::text IS NULL OR a.action LIKE $3)
                       AND ($4::text IS NULL OR a.actor_username ILIKE $4 OR a.target_label ILIKE $4)
                       AND ($5::timestamp IS NULL OR a.created_at >= $5)
                       AND ($6::timestamp IS NULL OR a.created_at < $6)",
                )
                .bind(query.actor_id)
                .bind(query.target_id)
                .bind(&action_pattern)
                .bind(&search)
                .bind(query.from)
                .bind(query.to)
                .fetch_one(&self.db)
                .await?
            }
            None => 0,
        };
        let entries = rows.into_iter().map(|row| row.entry).collect();

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
