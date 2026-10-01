//! The support desk. Accounts open tickets and talk to the staff there
//! (`/support/...`); staff answer them from the console
//! (`/admin/support/...`).

use crate::{
    database::{
        AppState,
        repositories::{
            audit_repository::AuditEvent,
            support_repository::{NewMessage, accepts_replies},
        },
    },
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse, PaginationQuery,
        admin::AdminListQuery,
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        notification::Notification,
        support::{
            AdminSupportTicket, AdminSupportTicketDetail, AdminTicketQuery, CreateTicketPayload,
            RateTicketPayload, ReplyTicketPayload, StaffReplyPayload, SupportMessage,
            SupportSummary, SupportTicket, SupportTicketDetail, TicketStatus, UpdateTicketPayload,
        },
    },
    services::notifier::notify,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Tickets one account can have open or pending at once.
pub const MAX_ACTIVE_TICKETS: i64 = 5;
/// Tickets one account can open in 24 hours.
pub const MAX_TICKETS_PER_DAY: i64 = 10;
/// Resolved tickets nobody replied to are closed after this many days.
pub const RESOLVED_TICKET_CLOSE_DAYS: i64 = 14;

/// The caller's support state, for the menu badge.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MySupportSummary {
    /// Tickets with a staff reply the caller hasn't opened.
    pub unread: i64,
    /// Tickets open or pending.
    pub active: i64,
}

fn ticket_limit(reason: &str, limit: i64) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::TOO_MANY_REQUESTS,
        codes::SUPPORT_TICKET_LIMIT,
        match reason {
            "open" => format!(
                "You already have {limit} open requests. Reply to one of them or wait for an answer."
            ),
            _ => format!("You can open at most {limit} requests a day."),
        },
        json!({ "reason": reason, "limit": limit }),
    )
}

fn ticket_closed() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::TICKET_CLOSED,
        "This request is closed. Open a new one if you still need help.",
    )
}

async fn own_ticket(
    state: &AppState,
    access: &AccessControl,
    id: Uuid,
) -> Result<SupportTicket, ApiError> {
    state
        .support_repo
        .find_for_user(access.user_id(), id)
        .await?
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    get,
    path = "/api/v1/support/tickets",
    tags = ["Support"],
    summary = "The caller's support requests, latest activity first.",
    params(PaginationQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Tickets.", body = PaginatedResponse<SupportTicket>))
)]
pub async fn list_my_tickets(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (page, per_page) = pagination.resolve();
    let (tickets, total) = state
        .support_repo
        .list_for_user(access.user_id(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(tickets, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/support/summary",
    tags = ["Support"],
    summary = "How many of the caller's requests have unread replies.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Summary.", body = MySupportSummary))
)]
pub async fn my_summary(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let (unread, active) = tokio::try_join!(
        state.support_repo.unread_for_user(access.user_id()),
        state.support_repo.count_active_for_user(access.user_id()),
    )?;
    Ok(Json(MySupportSummary { unread, active }))
}

#[utoipa::path(
    post,
    path = "/api/v1/support/tickets",
    tags = ["Support"],
    summary = "Open a support request.",
    description = "`subject` 3 to 150 characters, `body` up to 5 000. `category`: `account`, `billing`, `bug`, `feature`, `content` or `other` (billing and account requests start with a higher priority). `context` is an optional JSON object (at most 2 000 bytes) with where the request came from. At most 5 requests open at once and 10 a day (`SUPPORT_TICKET_LIMIT`, 429).",
    request_body = CreateTicketPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Opened.", body = SupportTicketDetail),
        (status = 429, description = "Too many requests open or opened today."),
    )
)]
pub async fn create_ticket(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateTicketPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user_id = access.user_id();
    let since = Utc::now().naive_utc() - Duration::hours(24);
    let (active, today) = tokio::try_join!(
        state.support_repo.count_active_for_user(user_id),
        state.support_repo.count_created_since(user_id, since),
    )?;
    if active >= MAX_ACTIVE_TICKETS {
        return Err(ticket_limit("open", MAX_ACTIVE_TICKETS));
    }
    if today >= MAX_TICKETS_PER_DAY {
        return Err(ticket_limit("daily", MAX_TICKETS_PER_DAY));
    }

    let ticket = state
        .support_repo
        .create(
            user_id,
            &access.0.username,
            &payload,
            payload.category.default_priority(),
        )
        .await?;
    let messages = state.support_repo.messages(ticket.id, false).await?;
    info!(ticket = ticket.number, %user_id, "Support request opened");
    Ok((
        StatusCode::CREATED,
        Json(SupportTicketDetail { ticket, messages }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/support/tickets/{id}",
    tags = ["Support"],
    summary = "One of the caller's requests with its conversation.",
    description = "Opening it marks the staff's replies as read. Internal staff notes are never included.",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Ticket.", body = SupportTicketDetail))
)]
pub async fn get_my_ticket(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let mut ticket = own_ticket(&state, &access, id).await?;
    // Staff viewing as the requester don't read on their behalf.
    if ticket.requester_unread && access.impersonator().is_none() {
        state.support_repo.mark_read_by_requester(id).await?;
        ticket.requester_unread = false;
    }
    let messages = state.support_repo.messages(id, false).await?;
    Ok(Json(SupportTicketDetail { ticket, messages }))
}

#[utoipa::path(
    post,
    path = "/api/v1/support/tickets/{id}/messages",
    tags = ["Support"],
    summary = "Reply to one of the caller's requests.",
    description = "Opens a pending or resolved request again (it waits for the staff). Closed requests take no replies (`TICKET_CLOSED`).",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    request_body = ReplyTicketPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Sent.", body = SupportMessage),
        (status = 409, description = "The ticket is closed."),
    )
)]
pub async fn reply_my_ticket(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<ReplyTicketPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let ticket = own_ticket(&state, &access, id).await?;
    if !accepts_replies(ticket.status) {
        return Err(ticket_closed());
    }
    let message = state
        .support_repo
        .add_message(NewMessage {
            ticket_id: id,
            author_id: access.user_id(),
            author_username: &access.0.username,
            from_staff: false,
            internal: false,
            body: &payload.body,
            status: Some(TicketStatus::Open),
        })
        .await?;
    Ok((StatusCode::CREATED, Json(message)))
}

#[utoipa::path(
    post,
    path = "/api/v1/support/tickets/{id}/close",
    tags = ["Support"],
    summary = "Close one of the caller's requests.",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Closed.", body = SupportTicket))
)]
pub async fn close_my_ticket(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    own_ticket(&state, &access, id).await?;
    state
        .support_repo
        .set_status(id, TicketStatus::Closed)
        .await?;
    Ok(Json(own_ticket(&state, &access, id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/support/tickets/{id}/rating",
    tags = ["Support"],
    summary = "Rate how a resolved request was handled.",
    description = "`rating` 1 to 5, with an optional `comment` (500 characters). Once per request, after it is resolved or closed (`TICKET_NOT_RATEABLE`, 409).",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    request_body = RateTicketPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Rated.", body = SupportTicket),
        (status = 409, description = "Not resolved yet, or already rated."),
    )
)]
pub async fn rate_my_ticket(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<RateTicketPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    own_ticket(&state, &access, id).await?;
    let comment = payload
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if !state
        .support_repo
        .rate(id, access.user_id(), payload.rating, comment)
        .await?
    {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::TICKET_NOT_RATEABLE,
            "Only a resolved request can be rated, once.",
        ));
    }
    Ok(Json(own_ticket(&state, &access, id).await?))
}

// ---------------------------------------------------------------------
// Staff
// ---------------------------------------------------------------------

async fn staff_ticket(state: &AppState, id: Uuid) -> Result<AdminSupportTicket, ApiError> {
    state
        .support_repo
        .admin_find(id)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn staff_detail(state: &AppState, id: Uuid) -> Result<AdminSupportTicketDetail, ApiError> {
    let ticket = staff_ticket(state, id).await?;
    let (messages, other_tickets) = tokio::try_join!(
        state.support_repo.messages(id, true),
        state.support_repo.other_tickets(ticket.user_id, id),
    )?;
    Ok(AdminSupportTicketDetail {
        ticket,
        messages,
        other_tickets,
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/support/tickets",
    tags = ["Support"],
    summary = "The support inbox (staff).",
    description = "Active requests (open and pending) by default, most urgent and longest-waiting first. `status` also takes `open`, `pending`, `resolved`, `closed` or `all`. `q` matches the ticket number (`#1042`), subject, username or e-mail.",
    params(AdminTicketQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Tickets.", body = PaginatedResponse<AdminSupportTicket>))
)]
pub async fn admin_list(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AdminTicketQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = crate::models::resolve_page(query.page, query.per_page, 25);
    let search = AdminListQuery {
        q: query.q.clone(),
        ..Default::default()
    }
    .search_pattern();
    let (tickets, total) = state
        .support_repo
        .admin_list(&query, search.as_deref(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(tickets, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/support/summary",
    tags = ["Support"],
    summary = "Inbox counts, satisfaction and response time (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Summary.", body = SupportSummary))
)]
pub async fn admin_summary(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.support_repo.summary(access.user_id()).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/support/tickets/{id}",
    tags = ["Support"],
    summary = "A request with its whole conversation, internal notes included (staff).",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Ticket.", body = AdminSupportTicketDetail))
)]
pub async fn admin_get(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(staff_detail(&state, id).await?))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/support/tickets/{id}",
    tags = ["Support"],
    summary = "Change a request's status, priority, category or assignee (staff).",
    description = "`assignee_id` must be a staff account; `null` unassigns.",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    request_body = UpdateTicketPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = AdminSupportTicketDetail))
)]
pub async fn admin_update(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateTicketPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let ticket = staff_ticket(&state, id).await?;
    if let Some(Some(assignee)) = payload.assignee_id {
        let staff = state
            .user_repo
            .find_by_id(assignee)
            .await?
            .is_some_and(|user| user.role.is_staff());
        if !staff {
            return Err(ApiError::BadRequest(
                "Requests can only be assigned to staff.".into(),
            ));
        }
    }
    state.support_repo.admin_update(id, &payload).await?;
    AuditEvent::by(&access, actions::SUPPORT_TICKET_UPDATED)
        .target("support_ticket", id, &format!("#{}", ticket.number))
        .meta(json!({
            "status": payload.status,
            "priority": payload.priority,
            "category": payload.category,
            "assignee_id": payload.assignee_id,
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(staff_detail(&state, id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/support/tickets/{id}/messages",
    tags = ["Support"],
    summary = "Answer a request, or leave an internal note (staff).",
    description = "A public reply sets the request to `pending` (or `status`, when given), notifies the requester in the app and by e-mail (their account e-mail preferences apply) and assigns the request to the caller when nobody has it. An `internal` note is only seen by staff. Closed requests take internal notes only (`TICKET_CLOSED`).",
    params(("id" = Uuid, Path, description = "Ticket UUID")),
    request_body = StaffReplyPayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Sent.", body = AdminSupportTicketDetail))
)]
pub async fn admin_reply(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<StaffReplyPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let ticket = staff_ticket(&state, id).await?;
    if !payload.internal && !accepts_replies(ticket.status) {
        return Err(ticket_closed());
    }
    let status = match (payload.internal, payload.status) {
        (_, Some(status)) => Some(status),
        (false, None) => Some(TicketStatus::Pending),
        (true, None) => None,
    };
    state
        .support_repo
        .add_message(NewMessage {
            ticket_id: id,
            author_id: access.user_id(),
            author_username: &access.0.username,
            from_staff: true,
            internal: payload.internal,
            body: &payload.body,
            status,
        })
        .await?;
    if !payload.internal && ticket.assignee_id.is_none() {
        state
            .support_repo
            .admin_update(
                id,
                &UpdateTicketPayload {
                    assignee_id: Some(Some(access.user_id())),
                    ..Default::default()
                },
            )
            .await?;
    }

    AuditEvent::by(&access, actions::SUPPORT_TICKET_REPLIED)
        .target("support_ticket", id, &format!("#{}", ticket.number))
        .meta(json!({ "internal": payload.internal, "status": status }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    if !payload.internal {
        notify(
            &state,
            Notification::support_reply(ticket.user_id, id, ticket.number, &ticket.subject),
        )
        .await;
    }
    Ok((StatusCode::CREATED, Json(staff_detail(&state, id).await?)))
}
