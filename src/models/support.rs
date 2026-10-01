//! The support desk: tickets opened by accounts and answered by staff.

use crate::models::user::Role;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::{Validate, ValidationError};

/// Longest message body, in characters.
pub const MAX_SUPPORT_MESSAGE_LENGTH: usize = 5_000;

/// Whose turn it is on a ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "support_ticket_status", rename_all = "snake_case")]
pub enum TicketStatus {
    /// Waiting for the staff.
    Open,
    /// Waiting for the requester.
    Pending,
    /// Solved; a reply from the requester opens it again.
    Resolved,
    /// Finished: no more replies.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "support_ticket_priority", rename_all = "snake_case")]
pub enum TicketPriority {
    Low,
    Normal,
    High,
    Urgent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "support_ticket_category", rename_all = "snake_case")]
pub enum TicketCategory {
    Account,
    Billing,
    Bug,
    Feature,
    Content,
    Other,
}

impl TicketCategory {
    /// The priority a new ticket of this category starts with.
    pub fn default_priority(&self) -> TicketPriority {
        match self {
            TicketCategory::Billing | TicketCategory::Account => TicketPriority::High,
            TicketCategory::Feature => TicketPriority::Low,
            _ => TicketPriority::Normal,
        }
    }
}

/// A ticket as its requester sees it.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SupportTicket {
    pub id: Uuid,
    pub number: i64,
    pub subject: String,
    pub category: TicketCategory,
    pub status: TicketStatus,
    pub rating: Option<i16>,
    pub rating_comment: Option<String>,
    /// Public messages (internal staff notes aren't counted).
    pub message_count: i32,
    pub last_message_at: NaiveDateTime,
    /// A staff reply the requester hasn't opened yet.
    pub requester_unread: bool,
    pub resolved_at: Option<NaiveDateTime>,
    pub closed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// One message of a ticket.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SupportMessage {
    pub id: Uuid,
    pub ticket_id: Uuid,
    pub author_id: Option<Uuid>,
    /// The author's username at the time (staff are shown as "Setlyst
    /// team" to the requester when it is `None`).
    pub author_username: Option<String>,
    pub from_staff: bool,
    /// Staff-only note; never sent to the requester.
    pub internal: bool,
    pub body: String,
    pub created_at: NaiveDateTime,
}

/// A ticket with its conversation (`GET /support/tickets/{id}`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SupportTicketDetail {
    pub ticket: SupportTicket,
    pub messages: Vec<SupportMessage>,
}

/// A ticket as the staff inbox lists it.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AdminSupportTicket {
    pub id: Uuid,
    pub number: i64,
    pub subject: String,
    pub category: TicketCategory,
    pub status: TicketStatus,
    pub priority: TicketPriority,
    pub user_id: Uuid,
    pub username: String,
    pub user_email: Option<String>,
    pub user_role: Role,
    pub user_avatar_url: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub assignee_username: Option<String>,
    pub context: Value,
    pub rating: Option<i16>,
    pub rating_comment: Option<String>,
    pub message_count: i32,
    pub last_message_at: NaiveDateTime,
    pub first_response_at: Option<NaiveDateTime>,
    /// Whether the last public message came from the requester (the
    /// ticket waits for staff).
    pub last_from_requester: bool,
    pub resolved_at: Option<NaiveDateTime>,
    pub closed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// A ticket with its whole conversation, internal notes included, and the
/// requester's other tickets.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminSupportTicketDetail {
    pub ticket: AdminSupportTicket,
    pub messages: Vec<SupportMessage>,
    /// The requester's latest other tickets (newest first, at most 10).
    pub other_tickets: Vec<SupportTicket>,
}

fn validate_body(body: &str) -> Result<(), ValidationError> {
    let length = body.trim().chars().count();
    if length == 0 || length > MAX_SUPPORT_MESSAGE_LENGTH {
        let mut error = ValidationError::new("length");
        error.message =
            Some(format!("Must be between 1 and {MAX_SUPPORT_MESSAGE_LENGTH} characters.").into());
        return Err(error);
    }
    Ok(())
}

fn validate_subject(subject: &str) -> Result<(), ValidationError> {
    let length = subject.trim().chars().count();
    if !(3..=150).contains(&length) {
        let mut error = ValidationError::new("length");
        error.message = Some("Must be between 3 and 150 characters.".into());
        return Err(error);
    }
    Ok(())
}

fn validate_context(context: &Value) -> Result<(), ValidationError> {
    let fits = context.is_object() && context.to_string().len() <= 2_000;
    if !fits {
        let mut error = ValidationError::new("context");
        error.message = Some("Must be a JSON object of at most 2 000 bytes.".into());
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct CreateTicketPayload {
    #[validate(custom(function = "validate_subject"))]
    pub subject: String,
    pub category: TicketCategory,
    #[validate(custom(function = "validate_body"))]
    pub body: String,
    /// Where the requester was (page, app version, browser...).
    #[serde(default)]
    #[validate(custom(function = "validate_context"))]
    #[schema(value_type = Object)]
    pub context: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct ReplyTicketPayload {
    #[validate(custom(function = "validate_body"))]
    pub body: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct RateTicketPayload {
    #[validate(range(min = 1, max = 5))]
    pub rating: i16,
    #[validate(length(max = 500))]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct StaffReplyPayload {
    #[validate(custom(function = "validate_body"))]
    pub body: String,
    /// A note for the staff only (the requester is not told).
    #[serde(default)]
    pub internal: bool,
    /// The ticket's status after the reply. Defaults to `pending` for a
    /// public reply and leaves it as is for an internal note.
    pub status: Option<TicketStatus>,
}

/// Staff changes to a ticket. Every field is optional; `assignee_id:
/// null` unassigns.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateTicketPayload {
    pub status: Option<TicketStatus>,
    pub priority: Option<TicketPriority>,
    pub category: Option<TicketCategory>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<Uuid>)]
    pub assignee_id: Option<Option<Uuid>>,
}

/// Filters of the staff inbox.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminTicketQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// `open`, `pending`, `resolved`, `closed`, or `active` (open and
    /// pending, the default) or `all`.
    pub status: Option<String>,
    pub priority: Option<TicketPriority>,
    pub category: Option<TicketCategory>,
    /// Only tickets assigned to this staff member.
    pub assignee_id: Option<Uuid>,
    /// `true`: only tickets nobody is assigned to.
    pub unassigned: Option<bool>,
    /// Only tickets of this account.
    pub user_id: Option<Uuid>,
    /// Ticket number, subject, username or e-mail.
    pub q: Option<String>,
}

/// Counts for the inbox tabs and the console's overview.
#[derive(Debug, Clone, Default, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SupportSummary {
    pub open: i64,
    pub pending: i64,
    pub resolved: i64,
    pub closed: i64,
    /// Open tickets nobody is assigned to.
    pub unassigned: i64,
    pub urgent: i64,
    /// Open or pending tickets assigned to the caller.
    pub mine: i64,
    /// Average rating over the last 90 days (1 to 5).
    pub average_rating: Option<f64>,
    pub ratings: i64,
    /// Median time to the first staff reply over the last 30 days, in
    /// minutes.
    pub median_first_response_minutes: Option<f64>,
}
