use crate::{
    errors::api_error::ApiError,
    models::support::{
        AdminSupportTicket, AdminTicketQuery, CreateTicketPayload, SupportMessage, SupportSummary,
        SupportTicket, TicketPriority, TicketStatus, UpdateTicketPayload,
    },
};
use chrono::{NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

/// Columns of a [`SupportTicket`] from `support_tickets t`.
macro_rules! ticket_columns {
    () => {
        "t.id, t.number, t.subject, t.category, t.status, t.rating, t.rating_comment,
         t.message_count, t.last_message_at, t.requester_unread, t.resolved_at, t.closed_at,
         t.created_at, t.updated_at"
    };
}

/// Columns of an [`AdminSupportTicket`] from `support_tickets t` joined
/// with its requester `u`.
macro_rules! admin_ticket_columns {
    () => {
        "t.id, t.number, t.subject, t.category, t.status, t.priority, t.user_id,
         u.username, u.email AS user_email, u.role AS user_role, u.avatar_url AS user_avatar_url,
         t.assignee_id, (SELECT a.username FROM users a WHERE a.id = t.assignee_id) AS assignee_username,
         t.context, t.rating, t.rating_comment, t.message_count, t.last_message_at,
         t.first_response_at,
         COALESCE((SELECT NOT m.from_staff FROM support_messages m
                   WHERE m.ticket_id = t.id AND NOT m.internal
                   ORDER BY m.created_at DESC, m.id DESC LIMIT 1), TRUE) AS last_from_requester,
         t.resolved_at, t.closed_at, t.created_at, t.updated_at
         FROM support_tickets t
         JOIN users u ON u.id = t.user_id"
    };
}

/// The inbox filter (`$1` status keyword, `$2` priority, `$3` category,
/// `$4` assignee, `$5` unassigned only, `$6` requester, `$7` escaped
/// search pattern, `$8` ticket number).
macro_rules! admin_ticket_filter {
    () => {
        " WHERE (CASE COALESCE($1, 'active')
                    WHEN 'all' THEN TRUE
                    WHEN 'active' THEN t.status IN ('open', 'pending')
                    ELSE t.status::text = $1
                 END)
            AND ($2::support_ticket_priority IS NULL OR t.priority = $2)
            AND ($3::support_ticket_category IS NULL OR t.category = $3)
            AND ($4::uuid IS NULL OR t.assignee_id = $4)
            AND ($5 = FALSE OR t.assignee_id IS NULL)
            AND ($6::uuid IS NULL OR t.user_id = $6)
            AND ($7::text IS NULL
                 OR t.subject ILIKE $7
                 OR u.username ILIKE $7
                 OR u.email ILIKE $7
                 OR t.number = $8)"
    };
}

/// Sets `status` and the timestamps that go with it (`$2` the status,
/// `$3` now). `resolved_at` restarts whenever the ticket becomes resolved
/// again: the automatic close counts from it.
macro_rules! status_assignments {
    () => {
        "status = $2,
         resolved_at = CASE WHEN $2 = 'resolved' AND status = 'resolved' THEN COALESCE(resolved_at, $3)
                            WHEN $2 = 'resolved' THEN $3
                            WHEN $2 IN ('open', 'pending') THEN NULL
                            ELSE resolved_at END,
         closed_at = CASE WHEN $2 = 'closed' THEN COALESCE(closed_at, $3) ELSE NULL END,
         updated_at = $3"
    };
}

/// A message about to be added to a ticket.
pub struct NewMessage<'a> {
    pub ticket_id: Uuid,
    pub author_id: Uuid,
    pub author_username: &'a str,
    pub from_staff: bool,
    pub internal: bool,
    pub body: &'a str,
    /// The ticket's status afterwards (`None`: unchanged).
    pub status: Option<TicketStatus>,
}

#[async_trait::async_trait]
pub trait SupportRepository: Send + Sync {
    /// Opens a ticket with its first message.
    async fn create(
        &self,
        user_id: Uuid,
        username: &str,
        payload: &CreateTicketPayload,
        priority: TicketPriority,
    ) -> Result<SupportTicket, ApiError>;
    /// Tickets of `user_id` still open or pending.
    async fn count_active_for_user(&self, user_id: Uuid) -> Result<i64, ApiError>;
    /// Tickets `user_id` opened since `since`.
    async fn count_created_since(
        &self,
        user_id: Uuid,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError>;
    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<SupportTicket>, i64), ApiError>;
    /// Tickets of `user_id` with a staff reply they haven't opened.
    async fn unread_for_user(&self, user_id: Uuid) -> Result<i64, ApiError>;
    async fn find_for_user(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<Option<SupportTicket>, ApiError>;
    async fn messages(
        &self,
        ticket_id: Uuid,
        include_internal: bool,
    ) -> Result<Vec<SupportMessage>, ApiError>;
    /// Adds a message and updates the ticket (status, counters, the
    /// requester's unread flag) in one transaction.
    async fn add_message(&self, message: NewMessage<'_>) -> Result<SupportMessage, ApiError>;
    async fn mark_read_by_requester(&self, id: Uuid) -> Result<(), ApiError>;
    async fn set_status(&self, id: Uuid, status: TicketStatus) -> Result<(), ApiError>;
    /// Rates a resolved or closed ticket of `user_id` that has no rating
    /// yet. `false` when it can't be rated.
    async fn rate(
        &self,
        id: Uuid,
        user_id: Uuid,
        rating: i16,
        comment: Option<&str>,
    ) -> Result<bool, ApiError>;
    async fn admin_list(
        &self,
        query: &AdminTicketQuery,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<AdminSupportTicket>, i64), ApiError>;
    async fn admin_find(&self, id: Uuid) -> Result<Option<AdminSupportTicket>, ApiError>;
    async fn admin_update(&self, id: Uuid, payload: &UpdateTicketPayload) -> Result<(), ApiError>;
    /// The latest tickets of `user_id` other than `exclude`.
    async fn other_tickets(
        &self,
        user_id: Uuid,
        exclude: Uuid,
    ) -> Result<Vec<SupportTicket>, ApiError>;
    async fn summary(&self, caller: Uuid) -> Result<SupportSummary, ApiError>;
    /// Closes resolved tickets nobody replied to for `days` days.
    async fn close_stale_resolved(&self, days: i64) -> Result<u64, ApiError>;
    /// Every ticket of `user_id` with its public messages (personal data
    /// export).
    async fn export_for_user(&self, user_id: Uuid) -> Result<Value, ApiError>;
}

pub struct SupportRepositoryImpl {
    pub db: PgPool,
}

impl SupportRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

#[async_trait::async_trait]
impl SupportRepository for SupportRepositoryImpl {
    async fn create(
        &self,
        user_id: Uuid,
        username: &str,
        payload: &CreateTicketPayload,
        priority: TicketPriority,
    ) -> Result<SupportTicket, ApiError> {
        let now = now();
        let id = Uuid::now_v7();
        let mut tx = self.db.begin().await?;
        sqlx::query(
            "INSERT INTO support_tickets (id, user_id, subject, category, status, priority, context,
                                          message_count, last_message_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'open', $5, $6, 1, $7, $7, $7)",
        )
        .bind(id)
        .bind(user_id)
        .bind(payload.subject.trim())
        .bind(payload.category)
        .bind(priority)
        .bind(payload.context.clone().unwrap_or_else(|| json!({})))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO support_messages (id, ticket_id, author_id, author_username, from_staff,
                                           internal, body, created_at)
             VALUES ($1, $2, $3, $4, FALSE, FALSE, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(user_id)
        .bind(username)
        .bind(payload.body.trim())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        let ticket = sqlx::query_as::<_, SupportTicket>(concat!(
            "SELECT ",
            ticket_columns!(),
            " FROM support_tickets t WHERE t.id = $1"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ticket)
    }

    async fn count_active_for_user(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM support_tickets
             WHERE user_id = $1 AND status IN ('open', 'pending')",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn count_created_since(
        &self,
        user_id: Uuid,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM support_tickets WHERE user_id = $1 AND created_at >= $2",
        )
        .bind(user_id)
        .bind(since)
        .fetch_one(&self.db)
        .await?)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<SupportTicket>, i64), ApiError> {
        let total = sqlx::query_scalar("SELECT COUNT(*) FROM support_tickets WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&self.db);
        let rows = sqlx::query_as::<_, SupportTicket>(concat!(
            "SELECT ",
            ticket_columns!(),
            " FROM support_tickets t WHERE t.user_id = $1
              ORDER BY t.last_message_at DESC, t.id DESC LIMIT $2 OFFSET $3"
        ))
        .bind(user_id)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);
        let (total, rows) = tokio::try_join!(total, rows)?;
        Ok((rows, total))
    }

    async fn unread_for_user(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM support_tickets WHERE user_id = $1 AND requester_unread",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn find_for_user(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<Option<SupportTicket>, ApiError> {
        Ok(sqlx::query_as::<_, SupportTicket>(concat!(
            "SELECT ",
            ticket_columns!(),
            " FROM support_tickets t WHERE t.id = $1 AND t.user_id = $2"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn messages(
        &self,
        ticket_id: Uuid,
        include_internal: bool,
    ) -> Result<Vec<SupportMessage>, ApiError> {
        Ok(sqlx::query_as::<_, SupportMessage>(
            "SELECT id, ticket_id, author_id, author_username, from_staff, internal, body, created_at
             FROM support_messages
             WHERE ticket_id = $1 AND ($2 OR NOT internal)
             ORDER BY created_at, id",
        )
        .bind(ticket_id)
        .bind(include_internal)
        .fetch_all(&self.db)
        .await?)
    }

    async fn add_message(&self, message: NewMessage<'_>) -> Result<SupportMessage, ApiError> {
        let now = now();
        let mut tx = self.db.begin().await?;
        // The caller checked the status already; checked again under the
        // row lock so a reply racing a close (by the other side or the
        // daily job) can't slip in and reopen a closed ticket.
        let current: Option<TicketStatus> =
            sqlx::query_scalar("SELECT status FROM support_tickets WHERE id = $1 FOR UPDATE")
                .bind(message.ticket_id)
                .fetch_optional(&mut *tx)
                .await?;
        match current {
            None => return Err(ApiError::NotFound),
            Some(status) if !message.internal && !accepts_replies(status) => {
                return Err(ApiError::ticket_closed());
            }
            Some(_) => {}
        }
        let row = sqlx::query_as::<_, SupportMessage>(
            "INSERT INTO support_messages (id, ticket_id, author_id, author_username, from_staff,
                                           internal, body, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, ticket_id, author_id, author_username, from_staff, internal, body, created_at",
        )
        .bind(Uuid::now_v7())
        .bind(message.ticket_id)
        .bind(message.author_id)
        .bind(message.author_username)
        .bind(message.from_staff)
        .bind(message.internal)
        .bind(message.body.trim())
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        if !message.internal {
            sqlx::query(
                "UPDATE support_tickets
                 SET message_count = message_count + 1,
                     last_message_at = $2,
                     first_response_at = CASE WHEN $3 THEN COALESCE(first_response_at, $2)
                                              ELSE first_response_at END,
                     requester_unread = $3,
                     updated_at = $2
                 WHERE id = $1",
            )
            .bind(message.ticket_id)
            .bind(now)
            .bind(message.from_staff)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(status) = message.status {
            sqlx::query(concat!(
                "UPDATE support_tickets SET ",
                status_assignments!(),
                " WHERE id = $1"
            ))
            .bind(message.ticket_id)
            .bind(status)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(row)
    }

    async fn mark_read_by_requester(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE support_tickets SET requester_unread = FALSE WHERE id = $1 AND requester_unread",
        )
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn set_status(&self, id: Uuid, status: TicketStatus) -> Result<(), ApiError> {
        sqlx::query(concat!(
            "UPDATE support_tickets SET ",
            status_assignments!(),
            " WHERE id = $1"
        ))
        .bind(id)
        .bind(status)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(())
    }

    async fn rate(
        &self,
        id: Uuid,
        user_id: Uuid,
        rating: i16,
        comment: Option<&str>,
    ) -> Result<bool, ApiError> {
        let updated = sqlx::query(
            "UPDATE support_tickets SET rating = $3, rating_comment = $4, updated_at = $5
             WHERE id = $1 AND user_id = $2 AND rating IS NULL
               AND status IN ('resolved', 'closed')",
        )
        .bind(id)
        .bind(user_id)
        .bind(rating)
        .bind(comment)
        .bind(now())
        .execute(&self.db)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    async fn admin_list(
        &self,
        query: &AdminTicketQuery,
        search: Option<&str>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<AdminSupportTicket>, i64), ApiError> {
        let number: Option<i64> = query
            .q
            .as_deref()
            .map(|q| q.trim().trim_start_matches('#'))
            .and_then(|q| q.parse().ok());
        let unassigned = query.unassigned.unwrap_or(false);
        let total = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM support_tickets t JOIN users u ON u.id = t.user_id",
            admin_ticket_filter!()
        ))
        .bind(query.status.as_deref())
        .bind(query.priority)
        .bind(query.category)
        .bind(query.assignee_id)
        .bind(unassigned)
        .bind(query.user_id)
        .bind(search)
        .bind(number)
        .fetch_one(&self.db);
        // Active first; those waiting for the staff before those waiting
        // for the requester; then most urgent and waiting longest.
        let rows = sqlx::query_as::<_, AdminSupportTicket>(concat!(
            "SELECT ",
            admin_ticket_columns!(),
            admin_ticket_filter!(),
            " ORDER BY (t.status IN ('open', 'pending')) DESC,
                       (t.status = 'open') DESC,
                       CASE t.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1
                                       WHEN 'normal' THEN 2 ELSE 3 END,
                       CASE WHEN t.status IN ('open', 'pending') THEN t.last_message_at END ASC,
                       t.last_message_at DESC, t.id DESC
              LIMIT $9 OFFSET $10"
        ))
        .bind(query.status.as_deref())
        .bind(query.priority)
        .bind(query.category)
        .bind(query.assignee_id)
        .bind(unassigned)
        .bind(query.user_id)
        .bind(search)
        .bind(number)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);
        let (total, rows) = tokio::try_join!(total, rows)?;
        Ok((rows, total))
    }

    async fn admin_find(&self, id: Uuid) -> Result<Option<AdminSupportTicket>, ApiError> {
        Ok(sqlx::query_as::<_, AdminSupportTicket>(concat!(
            "SELECT ",
            admin_ticket_columns!(),
            " WHERE t.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn admin_update(&self, id: Uuid, payload: &UpdateTicketPayload) -> Result<(), ApiError> {
        let now = now();
        let mut tx = self.db.begin().await?;
        if let Some(status) = payload.status {
            sqlx::query(concat!(
                "UPDATE support_tickets SET ",
                status_assignments!(),
                " WHERE id = $1"
            ))
            .bind(id)
            .bind(status)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE support_tickets
             SET priority = COALESCE($2, priority),
                 category = COALESCE($3, category),
                 assignee_id = CASE WHEN $4 THEN $5 ELSE assignee_id END,
                 updated_at = $6
             WHERE id = $1",
        )
        .bind(id)
        .bind(payload.priority)
        .bind(payload.category)
        .bind(payload.assignee_id.is_some())
        .bind(payload.assignee_id.flatten())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn other_tickets(
        &self,
        user_id: Uuid,
        exclude: Uuid,
    ) -> Result<Vec<SupportTicket>, ApiError> {
        Ok(sqlx::query_as::<_, SupportTicket>(concat!(
            "SELECT ",
            ticket_columns!(),
            " FROM support_tickets t WHERE t.user_id = $1 AND t.id <> $2
              ORDER BY t.created_at DESC LIMIT 10"
        ))
        .bind(user_id)
        .bind(exclude)
        .fetch_all(&self.db)
        .await?)
    }

    async fn summary(&self, caller: Uuid) -> Result<SupportSummary, ApiError> {
        let now = now();
        Ok(sqlx::query_as::<_, SupportSummary>(
            "SELECT
                COUNT(*) FILTER (WHERE status = 'open') AS open,
                COUNT(*) FILTER (WHERE status = 'pending') AS pending,
                COUNT(*) FILTER (WHERE status = 'resolved') AS resolved,
                COUNT(*) FILTER (WHERE status = 'closed') AS closed,
                COUNT(*) FILTER (WHERE status = 'open' AND assignee_id IS NULL) AS unassigned,
                COUNT(*) FILTER (WHERE status IN ('open', 'pending') AND priority = 'urgent') AS urgent,
                COUNT(*) FILTER (WHERE status IN ('open', 'pending') AND assignee_id = $1) AS mine,
                AVG(rating) FILTER (WHERE rating IS NOT NULL
                                      AND COALESCE(resolved_at, closed_at, created_at) >= $2)::float8
                    AS average_rating,
                COUNT(*) FILTER (WHERE rating IS NOT NULL
                                   AND COALESCE(resolved_at, closed_at, created_at) >= $2) AS ratings,
                (PERCENTILE_CONT(0.5) WITHIN GROUP (
                    ORDER BY EXTRACT(EPOCH FROM (first_response_at - created_at)) / 60.0
                 ) FILTER (WHERE first_response_at IS NOT NULL AND created_at >= $3))::float8
                    AS median_first_response_minutes
             FROM support_tickets",
        )
        .bind(caller)
        .bind(now - chrono::Duration::days(90))
        .bind(now - chrono::Duration::days(30))
        .fetch_one(&self.db)
        .await?)
    }

    async fn close_stale_resolved(&self, days: i64) -> Result<u64, ApiError> {
        let now = now();
        let closed = sqlx::query(
            "UPDATE support_tickets
             SET status = 'closed', closed_at = $1, updated_at = $1
             WHERE status = 'resolved'
               AND GREATEST(COALESCE(resolved_at, last_message_at), last_message_at) < $2",
        )
        .bind(now)
        .bind(now - chrono::Duration::days(days))
        .execute(&self.db)
        .await?;
        Ok(closed.rows_affected())
    }

    async fn export_for_user(&self, user_id: Uuid) -> Result<Value, ApiError> {
        let tickets: Option<Value> = sqlx::query_scalar(
            "SELECT COALESCE(jsonb_agg(jsonb_build_object(
                    'number', t.number, 'subject', t.subject, 'category', t.category,
                    'status', t.status, 'priority', t.priority, 'context', t.context,
                    'rating', t.rating, 'rating_comment', t.rating_comment,
                    'created_at', t.created_at, 'resolved_at', t.resolved_at,
                    'closed_at', t.closed_at,
                    'messages', (SELECT COALESCE(jsonb_agg(jsonb_build_object(
                                     'from_staff', m.from_staff, 'body', m.body,
                                     'created_at', m.created_at) ORDER BY m.created_at), '[]'::jsonb)
                                 FROM support_messages m
                                 WHERE m.ticket_id = t.id AND NOT m.internal)
                ) ORDER BY t.created_at), '[]'::jsonb)
             FROM support_tickets t WHERE t.user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?;
        Ok(tickets.unwrap_or_else(|| json!([])))
    }
}

/// Whether `status` lets the requester reply (closed tickets don't).
pub fn accepts_replies(status: TicketStatus) -> bool {
    status != TicketStatus::Closed
}
