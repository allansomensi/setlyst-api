use crate::validations::{
    name::{validate_first_name, validate_last_name},
    password::validate_password,
    username::validate_username,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, PartialEq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Moderator,
    Admin,
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

#[derive(ToSchema, PartialEq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_status", rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Inactive,
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    /// When the username was last changed — `None` if it has never been
    /// changed since the account was created. Governs the 90-day cooldown
    /// between changes; see [`crate::database::repositories::user_repository::UserRepository::update`].
    pub username_changed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl User {
    pub fn new(
        username: &str,
        email: Option<String>,
        password: &str,
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
            password_hash: password.to_string(),
            first_name,
            last_name,
            role: role.unwrap_or_default(),
            status: status.unwrap_or_default(),
            username_changed_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(ToSchema, Clone, FromRow, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    /// When the username was last changed — `None` if never changed.
    /// The frontend uses this to compute the 90-day cooldown and show
    /// when the user is next allowed to change it again.
    pub username_changed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<User> for UserPublic {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            email: user.email,
            first_name: user.first_name,
            last_name: user.last_name,
            role: user.role,
            status: user.status,
            username_changed_at: user.username_changed_at,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct RegisterPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: String,

    #[validate(custom(function = "validate_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_last_name"))]
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
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: String,

    #[validate(custom(function = "validate_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_last_name"))]
    pub last_name: Option<String>,

    pub role: Option<Role>,

    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: Option<String>,

    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: Option<String>,

    #[validate(custom(function = "validate_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_last_name"))]
    pub last_name: Option<String>,

    pub role: Option<Role>,

    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateCurrentUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: Option<String>,

    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_last_name"))]
    pub last_name: Option<String>,
}

impl From<UpdateCurrentUserPayload> for UpdateUserPayload {
    fn from(payload: UpdateCurrentUserPayload) -> Self {
        Self {
            username: payload.username,
            email: payload.email,
            password: None,
            first_name: payload.first_name,
            last_name: payload.last_name,
            role: None,
            status: None,
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct ChangePasswordPayload {
    #[validate(length(min = 1, message = "Current password is required."))]
    pub current_password: String,

    #[validate(custom(function = "validate_password"))]
    pub new_password: String,
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
}

/// Another user's profile, as seen by the caller. Regular users see only
/// the basic public fields; admins additionally see the privileged block.
/// Built by [`UserPublic::into_profile_view`] — never constructed by hand,
/// so the redaction rule lives in exactly one place.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserProfileView {
    pub id: Uuid,
    pub username: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub created_at: NaiveDateTime,
    /// Set only when the caller is an admin — `None` for every other viewer.
    pub admin_details: Option<UserProfileAdminDetails>,
}

/// Privileged fields shown only to admins viewing someone else's profile.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserProfileAdminDetails {
    pub email: Option<String>,
    pub role: Role,
    pub status: Status,
    pub username_changed_at: Option<NaiveDateTime>,
}

impl UserPublic {
    /// Builds the profile view of `self` as seen by a caller who is (or
    /// isn't) an admin. This is the one place that decides which fields a
    /// non-admin viewer never sees.
    pub fn into_profile_view(self, viewer_is_admin: bool) -> UserProfileView {
        let admin_details = viewer_is_admin.then_some(UserProfileAdminDetails {
            email: self.email,
            role: self.role,
            status: self.status,
            username_changed_at: self.username_changed_at,
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
