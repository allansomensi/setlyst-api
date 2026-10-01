//! The staff console's home: what needs attention, and search across
//! the platform.

use crate::models::{
    platform::MaintenanceMode,
    support::{TicketPriority, TicketStatus},
    user::Role,
};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Clone, Default, FromRow, Serialize, Deserialize, ToSchema)]
pub struct UserCounts {
    pub total: i64,
    pub new_today: i64,
    pub new_7d: i64,
    pub new_30d: i64,
    /// Signed in during the last 7 days.
    pub active_7d: i64,
    /// Signed in during the last 30 days.
    pub active_30d: i64,
    pub unverified: i64,
    /// Suspended right now.
    pub banned: i64,
    pub deactivated: i64,
    /// Staff without two-factor authentication.
    pub staff_without_2fa: i64,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct DailyCount {
    pub day: NaiveDate,
    pub count: i64,
}

/// `GET /admin/overview`: what needs attention right now.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConsoleOverview {
    pub maintenance_mode: MaintenanceMode,
    pub registrations_open: bool,
    pub users: UserCounts,
    /// Sign-ups per day over the last 30 days (UTC, oldest first, days
    /// without sign-ups included).
    pub signups: Vec<DailyCount>,
    /// Open support requests (waiting for staff).
    pub support_open: i64,
    pub support_unassigned: i64,
    pub support_urgent: i64,
    /// Open moderation flags.
    pub moderation_open: i64,
    pub emails_failed_24h: i64,
    pub emails_pending: i64,
    pub oldest_pending_email_at: Option<NaiveDateTime>,
    /// Unresolved status-page incidents.
    pub incidents_active: i64,
    pub generated_at: NaiveDateTime,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ConsoleSearchQuery {
    /// At least 2 characters.
    pub q: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SearchUser {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub role: Role,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SearchBand {
    pub id: Uuid,
    pub name: String,
    pub logo_url: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SearchContent {
    pub id: Uuid,
    pub title: String,
    /// The artist (songs) or nothing (setlists).
    pub subtitle: Option<String>,
    pub owner_username: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SearchTicket {
    pub id: Uuid,
    pub number: i64,
    pub subject: String,
    pub status: TicketStatus,
    pub priority: TicketPriority,
    pub username: String,
}

/// `GET /admin/search`: the first matches of each kind (5 at most).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ConsoleSearchResults {
    pub users: Vec<SearchUser>,
    pub bands: Vec<SearchBand>,
    pub songs: Vec<SearchContent>,
    pub setlists: Vec<SearchContent>,
    pub tickets: Vec<SearchTicket>,
}

/// What `POST /admin/users/bulk` does to each account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkUserAction {
    /// Signs the accounts out everywhere.
    RevokeSessions,
    /// Suspends them (`duration_hours`, `reason`).
    Ban,
    Unban,
    /// Deactivates them.
    Deactivate,
    /// Reactivates them.
    Activate,
}

#[derive(Debug, Clone, Deserialize, ToSchema, validator::Validate)]
pub struct BulkUserPayload {
    pub action: BulkUserAction,
    /// 1 to 100 accounts.
    #[validate(length(min = 1, max = 100))]
    pub user_ids: Vec<Uuid>,
    /// For `ban`: hours (omitted = permanent).
    #[validate(range(min = 1, max = 87_600))]
    pub duration_hours: Option<i64>,
    /// For `ban`.
    #[validate(length(max = 500))]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkUserFailure {
    pub user_id: Uuid,
    /// The error code (`INSUFFICIENT_ROLE`, `CANNOT_TARGET_SELF`,
    /// `NOT_FOUND`, `LAST_ADMIN`...).
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkUserResult {
    pub succeeded: Vec<Uuid>,
    pub failed: Vec<BulkUserFailure>,
}

/// One sign-in attempt on the caller's account (`/users/me/sign-ins`).
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SignInEvent {
    /// `succeeded`, `failed` or `locked`.
    pub outcome: String,
    /// `password`, `google`, `two_factor`... (successful sign-ins).
    pub method: Option<String>,
    /// Whether it was the second step (a wrong second factor).
    pub second_factor: bool,
    /// Kept for six months (Marco Civil da Internet, art. 15).
    pub ip_address: Option<String>,
    pub created_at: NaiveDateTime,
}
