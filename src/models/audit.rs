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

    // Accounts (v0.12).
    pub const USER_SELF_DELETED: &str = "user.self_deleted";
    pub const USER_EMAIL_CHANGED: &str = "user.email_changed";
    pub const USER_TWO_FACTOR_ENABLED: &str = "user.two_factor_enabled";
    pub const USER_TWO_FACTOR_DISABLED: &str = "user.two_factor_disabled";
    /// A wrong second factor outside sign-in (disabling 2FA, regenerating
    /// recovery codes); `meta.context` says which.
    pub const USER_SECOND_FACTOR_FAILED: &str = "user.second_factor_failed";
    pub const USER_PASSWORD_RECOVERED: &str = "user.password_recovered";
    pub const USER_LOGIN_LOCKED: &str = "user.login_locked";
    pub const USER_TERMS_ACCEPTED: &str = "user.terms_accepted";
    pub const USER_SUBSCRIPTION_GRANTED: &str = "user.subscription_granted";
    pub const USER_SUBSCRIPTION_REVOKED: &str = "user.subscription_revoked";
    pub const USER_CREDITS_ADJUSTED: &str = "user.credits_adjusted";

    // Billing.
    pub const BILLING_SETTINGS_UPDATED: &str = "billing.settings_updated";
    pub const BILLING_PLAN_UPDATED: &str = "billing.plan_updated";
    pub const BILLING_TRIALS_GRANTED: &str = "billing.trials_granted";
    pub const FINANCE_SYNCED: &str = "finance.synced";
    pub const PROMO_CREATED: &str = "promo.created";
    pub const PROMO_UPDATED: &str = "promo.updated";
    pub const PROMOTION_CREATED: &str = "promotion.created";
    pub const PROMOTION_UPDATED: &str = "promotion.updated";
    pub const PROMOTION_DELETED: &str = "promotion.deleted";

    // Communications.
    pub const ANNOUNCEMENT_CREATED: &str = "announcement.created";
    pub const ANNOUNCEMENT_UPDATED: &str = "announcement.updated";
    pub const ANNOUNCEMENT_PUBLISHED: &str = "announcement.published";
    pub const ANNOUNCEMENT_ARCHIVED: &str = "announcement.archived";
    pub const ANNOUNCEMENT_DELETED: &str = "announcement.deleted";
    pub const RELEASE_NOTE_CREATED: &str = "release_note.created";
    pub const RELEASE_NOTE_UPDATED: &str = "release_note.updated";
    pub const RELEASE_NOTE_PUBLISHED: &str = "release_note.published";
    pub const RELEASE_NOTE_UNPUBLISHED: &str = "release_note.unpublished";
    pub const RELEASE_NOTE_DELETED: &str = "release_note.deleted";

    // Moderation.
    pub const MODERATION_DISMISSED: &str = "moderation.dismissed";
    pub const MODERATION_AVATAR_REMOVED: &str = "moderation.avatar_removed";
    pub const MODERATION_BAND_LOGO_REMOVED: &str = "moderation.band_logo_removed";
    pub const MODERATION_USERNAME_RESET: &str = "moderation.username_reset";
    pub const MODERATION_RESCAN: &str = "moderation.rescan";

    // Account security (launch hardening).
    /// A wrong password or re-auth code typed to confirm a sensitive
    /// action while signed in; `meta.context` says which.
    pub const USER_REAUTH_FAILED: &str = "user.reauth_failed";
    /// Too many wrong confirmations in 24 hours: every session was
    /// signed out.
    pub const USER_REAUTH_SESSIONS_REVOKED: &str = "user.reauth_sessions_revoked";
    pub const USER_PASSWORD_RESET_FAILED: &str = "user.password_reset_failed";
    pub const USER_LOGIN_SUCCEEDED: &str = "user.login_succeeded";
    pub const USER_IDENTITY_LINKED: &str = "user.identity_linked";
    pub const USER_IDENTITY_UNLINKED: &str = "user.identity_unlinked";
    pub const USER_RECOVERY_CODES_REGENERATED: &str = "user.recovery_codes_regenerated";
    pub const USER_EMAIL_CHANGE_STARTED: &str = "user.email_change_started";
    /// An unverified account lost its (never proven) address to the
    /// person who proved they own it (Google sign-in).
    pub const USER_EMAIL_DETACHED: &str = "user.email_detached";
    /// First proof of ownership of the address through password recovery:
    /// what the unproven owner set up (2FA, linked sign-ins) was removed.
    pub const USER_SECURITY_RESET: &str = "user.security_reset";
    pub const USER_COMMUNICATION_CHANGED: &str = "user.communication_changed";
    /// Unverified account without content, deleted after 7 days.
    pub const USER_UNVERIFIED_PURGED: &str = "user.unverified_purged";
    /// A request made while viewing the platform as the target;
    /// `meta.blocked` when it was refused (exports).
    pub const USER_IMPERSONATED_READ: &str = "user.impersonated_read";
    /// Staff opened private content (a song, a setlist, an account
    /// overview).
    pub const STAFF_CONTENT_VIEWED: &str = "staff.content_viewed";
}

/// Documents a consent can be given to (`legal_acceptances.document`).
pub mod legal_documents {
    pub const TERMS_OF_USE: &str = "terms_of_use";
    pub const PRIVACY_POLICY: &str = "privacy_policy";
    pub const AGE_DECLARATION: &str = "age_declaration";
    pub const MARKETING_EMAIL: &str = "marketing_email";
}

/// One consent given (or withdrawn), with its evidence (LGPD art. 8, § 2).
#[derive(Debug, Clone)]
pub struct LegalAcceptance {
    pub user_id: Uuid,
    pub document: &'static str,
    pub version: String,
    pub accepted: bool,
    /// `register`, `google_signup`, `accept_terms`, `settings`,
    /// `unsubscribe_link`.
    pub source: &'static str,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
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
