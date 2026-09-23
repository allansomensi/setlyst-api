use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::prelude::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Stable action identifiers recorded in the audit log. Clients translate
/// them, so never rename one — add a new one instead.
pub mod actions {
    pub const USER_REGISTERED: &str = "user.registered";
    pub const USER_CREATED: &str = "user.created";
    pub const USER_UPDATED: &str = "user.updated";
    pub const USER_ROLE_CHANGED: &str = "user.role_changed";
    pub const USER_ACTIVATED: &str = "user.activated";
    pub const USER_DEACTIVATED: &str = "user.deactivated";
    pub const USER_BANNED: &str = "user.banned";
    pub const USER_UNBANNED: &str = "user.unbanned";
    pub const USER_DELETED: &str = "user.deleted";
    pub const USER_PASSWORD_CHANGED: &str = "user.password_changed";
    pub const USER_PASSWORD_RESET: &str = "user.password_reset";
    pub const USER_SESSIONS_REVOKED: &str = "user.sessions_revoked";
    pub const USER_QUOTAS_UPDATED: &str = "user.quotas_updated";
    pub const USER_IMPERSONATED: &str = "user.impersonated";
    pub const USER_LOGIN_FAILED: &str = "user.login_failed";
    pub const BAND_UPDATED: &str = "band.updated";
    pub const BAND_DELETED: &str = "band.deleted";
    pub const BAND_MEMBER_ADDED: &str = "band.member_added";
    pub const BAND_MEMBER_REMOVED: &str = "band.member_removed";
    pub const BAND_MEMBER_ROLE_CHANGED: &str = "band.member_role_changed";
    pub const BAND_OWNERSHIP_TRANSFERRED: &str = "band.ownership_transferred";
    pub const SONG_UPDATED: &str = "song.updated";
    pub const SONG_DELETED: &str = "song.deleted";
    pub const SETLIST_UPDATED: &str = "setlist.updated";
    pub const SETLIST_DELETED: &str = "setlist.deleted";
    pub const SHARE_REVOKED: &str = "share.revoked";
    pub const SHARE_UNLOCKED: &str = "share.unlocked";
    pub const QUOTA_DEFAULTS_UPDATED: &str = "settings.quota_defaults_updated";
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_username: Option<String>,
    pub impersonator_id: Option<Uuid>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<Uuid>,
    pub target_label: Option<String>,
    pub metadata: Value,
    pub ip_address: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditLogQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Only entries performed by this user.
    pub actor_id: Option<Uuid>,
    /// Only entries about this record (user, band, setlist...).
    pub target_id: Option<Uuid>,
    /// Exact action (e.g. `user.banned`) or a prefix ending in `.` (e.g. `user.`).
    pub action: Option<String>,
    /// Free-text search over actor/target labels.
    pub q: Option<String>,
}
