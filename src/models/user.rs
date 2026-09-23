use crate::validations::{
    name::{validate_first_name, validate_last_name},
    password::validate_password,
    text::validate_reason,
    username::validate_username,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use std::borrow::Cow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidateEmail, ValidationError};

#[derive(ToSchema, PartialEq, Eq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Moderator,
    Admin,
}

impl Role {
    /// Rank used for "may act on" comparisons: staff can only manage
    /// accounts strictly below their own rank.
    pub fn rank(&self) -> u8 {
        match self {
            Role::User => 0,
            Role::Moderator => 1,
            Role::Admin => 2,
        }
    }

    pub fn is_staff(&self) -> bool {
        matches!(self, Role::Moderator | Role::Admin)
    }

    /// `true` when a holder of `self` may manage an account holding
    /// `target`. Admins can manage moderators and users; moderators can
    /// only manage regular users; nobody manages a peer or a superior.
    pub fn outranks(&self, target: &Role) -> bool {
        self.is_staff() && self.rank() > target.rank()
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::User => "user",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
        };
        f.write_str(s)
    }
}

#[derive(ToSchema, PartialEq, Eq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_status", rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Inactive,
}

/// Returns the active suspension described by `banned_at`/`banned_until`,
/// if any: `Some(None)` is permanent, `Some(Some(until))` is temporary,
/// `None` means not suspended (never, or already expired).
pub fn active_ban(
    banned_at: Option<NaiveDateTime>,
    banned_until: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> Option<Option<NaiveDateTime>> {
    banned_at?;
    match banned_until {
        None => Some(None),
        Some(until) if until > now => Some(Some(until)),
        Some(_) => None,
    }
}

/// The full account row, including secrets. Never serialized to clients —
/// use [`UserPublic`] for responses.
#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    pub token_version: i32,
    pub must_change_password: bool,
    pub banned_at: Option<NaiveDateTime>,
    pub banned_until: Option<NaiveDateTime>,
    pub ban_reason: Option<String>,
    /// When the username was last changed — `None` if it has never been
    /// changed since the account was created. Governs the 90-day cooldown
    /// between changes.
    pub username_changed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl User {
    pub fn new(
        username: &str,
        email: Option<String>,
        password_hash: &str,
        first_name: Option<String>,
        last_name: Option<String>,
        role: Option<Role>,
        status: Option<Status>,
    ) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            username: username.to_string(),
            email,
            password_hash: password_hash.to_string(),
            first_name,
            last_name,
            role: role.unwrap_or_default(),
            status: status.unwrap_or_default(),
            token_version: 0,
            must_change_password: false,
            banned_at: None,
            banned_until: None,
            ban_reason: None,
            username_changed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn active_ban(&self) -> Option<Option<NaiveDateTime>> {
        active_ban(self.banned_at, self.banned_until, Utc::now().naive_utc())
    }
}

/// An account as returned to clients — never includes the password hash.
#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    /// When the username was last changed — `None` if never changed.
    pub username_changed_at: Option<NaiveDateTime>,
    pub must_change_password: bool,
    pub password_changed_at: Option<NaiveDateTime>,
    pub last_login_at: Option<NaiveDateTime>,
    /// `true` while a suspension is in effect (computed server-side so
    /// clients don't depend on their own clock).
    pub is_banned: bool,
    pub banned_at: Option<NaiveDateTime>,
    /// `None` with `is_banned` means a permanent suspension.
    pub banned_until: Option<NaiveDateTime>,
    pub ban_reason: Option<String>,
    pub banned_by_username: Option<String>,
    pub created_by_username: Option<String>,
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

fn validate_optional_email(email: &str) -> Result<(), ValidationError> {
    if email.is_empty() || (email.len() <= 100 && email.validate_email()) {
        return Ok(());
    }
    let mut error = ValidationError::new("email");
    error.message = Some(Cow::from("Invalid email address."));
    Err(error)
}

/// Empty strings clear optional name fields, so they skip validation.
fn validate_optional_first_name(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    validate_first_name(value)
}

fn validate_optional_last_name(value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    validate_last_name(value)
}

/// Normalizes an optional text field from a PATCH-style payload:
/// `None` = leave unchanged, `Some("")` (after trimming) = clear.
pub fn clearable(value: &Option<String>) -> Option<Option<String>> {
    value.as_ref().map(|v| {
        let trimmed = v.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct RegisterPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    #[validate(custom(function = "validate_optional_email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: String,

    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,
}

impl From<RegisterPayload> for CreateUserPayload {
    fn from(value: RegisterPayload) -> Self {
        Self {
            username: value.username,
            email: value.email,
            password: value.password,
            first_name: value.first_name,
            last_name: value.last_name,
            role: Some(Role::default()),
            status: Some(Status::default()),
            require_password_change: Some(false),
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    #[validate(custom(function = "validate_optional_email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: String,

    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,

    pub role: Option<Role>,

    pub status: Option<Status>,

    /// Forces the new account to pick its own password at first sign-in.
    /// Defaults to `true` for staff-created accounts: the creator knows
    /// the initial password, so it must be treated as temporary.
    pub require_password_change: Option<bool>,
}

/// Staff edit of another account. Passwords are deliberately absent — see
/// [`AdminResetPasswordPayload`], which also revokes sessions and forces a
/// change at next sign-in.
#[derive(Deserialize, Serialize, ToSchema, Validate, Default)]
pub struct UpdateUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: Option<String>,

    /// Empty string clears the email.
    #[validate(custom(function = "validate_optional_email"))]
    pub email: Option<String>,

    /// Empty string clears the field.
    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    /// Empty string clears the field.
    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,

    pub role: Option<Role>,

    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateCurrentUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: Option<String>,

    /// Empty string clears the email.
    #[validate(custom(function = "validate_optional_email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,
}

impl From<UpdateCurrentUserPayload> for UpdateUserPayload {
    fn from(payload: UpdateCurrentUserPayload) -> Self {
        Self {
            username: payload.username,
            email: payload.email,
            first_name: payload.first_name,
            last_name: payload.last_name,
            role: None,
            status: None,
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct ChangePasswordPayload {
    #[validate(length(min = 1, max = 256, message = "Current password is required."))]
    pub current_password: String,

    #[validate(custom(function = "validate_password"))]
    pub new_password: String,
}

#[derive(Deserialize, Serialize, ToSchema, Debug)]
pub struct ChangePasswordResponse {
    /// Always `true`: every session (including the caller's) was revoked,
    /// so the client must sign in again with the new password.
    pub reauth_required: bool,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct AdminResetPasswordPayload {
    #[validate(custom(function = "validate_password"))]
    pub new_password: String,

    /// Whether the user must replace this password at next sign-in.
    /// Defaults to `true` — a password someone else chose is temporary.
    pub require_change: Option<bool>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct BanUserPayload {
    /// Suspension length in hours; omit (or null) for a permanent ban.
    #[validate(range(
        min = 1,
        max = 87_600,
        message = "Duration must be between 1 hour and 10 years."
    ))]
    pub duration_hours: Option<i64>,

    #[validate(custom(function = "validate_reason"))]
    pub reason: Option<String>,
}

/// One past username a user has held, kept for admins to trace an
/// account across a rename. Never exposed to non-admins.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct UsernameHistoryEntry {
    pub old_username: String,
    pub changed_at: NaiveDateTime,
}

/// Response for the live username-availability check shown in Settings.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UsernameAvailability {
    pub available: bool,
    /// Set when the name is syntactically invalid or reserved, so the UI
    /// can explain why instead of just saying "unavailable".
    pub reason: Option<String>,
}

/// Another user's profile, as seen by the caller. Regular users see only
/// the basic public fields; staff additionally see the privileged block.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserProfileView {
    pub id: Uuid,
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub created_at: NaiveDateTime,
    /// Set only when the caller is staff — `None` for every other viewer.
    pub admin_details: Option<UserProfileAdminDetails>,
}

/// Privileged fields shown only to staff viewing someone else's profile.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserProfileAdminDetails {
    pub email: Option<String>,
    pub role: Role,
    pub status: Status,
    pub username_changed_at: Option<NaiveDateTime>,
    pub is_banned: bool,
    pub banned_until: Option<NaiveDateTime>,
    pub last_login_at: Option<NaiveDateTime>,
}

impl UserPublic {
    /// Builds the profile view of `self` as seen by a caller who is (or
    /// isn't) staff. This is the one place that decides which fields a
    /// regular viewer never sees.
    pub fn into_profile_view(self, viewer_is_staff: bool) -> UserProfileView {
        let admin_details = viewer_is_staff.then_some(UserProfileAdminDetails {
            email: self.email,
            role: self.role,
            status: self.status,
            username_changed_at: self.username_changed_at,
            is_banned: self.is_banned,
            banned_until: self.banned_until,
            last_login_at: self.last_login_at,
        });

        UserProfileView {
            id: self.id,
            username: self.username,
            first_name: self.first_name,
            last_name: self.last_name,
            created_at: self.created_at,
            admin_details,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn role_hierarchy() {
        assert!(Role::Admin.outranks(&Role::Moderator));
        assert!(Role::Admin.outranks(&Role::User));
        assert!(Role::Moderator.outranks(&Role::User));
        assert!(!Role::Moderator.outranks(&Role::Admin));
        assert!(!Role::Moderator.outranks(&Role::Moderator));
        assert!(!Role::Admin.outranks(&Role::Admin));
        assert!(!Role::User.outranks(&Role::User));
    }

    #[test]
    fn ban_windows() {
        let now = Utc::now().naive_utc();
        assert_eq!(active_ban(None, None, now), None);
        assert_eq!(active_ban(Some(now), None, now), Some(None));
        let future = now + Duration::hours(2);
        assert_eq!(active_ban(Some(now), Some(future), now), Some(Some(future)));
        let past = now - Duration::hours(2);
        assert_eq!(active_ban(Some(now), Some(past), now), None);
    }

    #[test]
    fn clearable_semantics() {
        assert_eq!(clearable(&None), None);
        assert_eq!(clearable(&Some("  ".into())), Some(None));
        assert_eq!(clearable(&Some(" Ana ".into())), Some(Some("Ana".into())));
    }

    #[test]
    fn optional_email_allows_clearing() {
        assert!(validate_optional_email("").is_ok());
        assert!(validate_optional_email("ana@example.com").is_ok());
        assert!(validate_optional_email("not-an-email").is_err());
    }
}

/// Query string of `GET /users`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UserListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Case-insensitive search over username, email and names.
    pub q: Option<String>,
}

impl UserListQuery {
    /// `q` as an escaped `ILIKE` pattern, or `None` when blank.
    pub fn search_pattern(&self) -> Option<String> {
        crate::models::admin::AdminListQuery {
            q: self.q.clone(),
            ..Default::default()
        }
        .search_pattern()
    }
}
