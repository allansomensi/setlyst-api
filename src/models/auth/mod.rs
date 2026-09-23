use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

pub mod access;
pub mod token;

/// Sign-in credentials. Only bounded in length here — strength rules are
/// deliberately *not* applied at sign-in, so an account whose password
/// predates the current policy can still sign in and be walked through a
/// mandatory change (see `controllers::auth::login`).
#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct LoginPayload {
    #[validate(length(min = 1, max = 64, message = "Username is required."))]
    pub username: String,

    #[validate(length(min = 1, max = 256, message = "Password is required."))]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginResponse {
    pub token: String,
    /// `true` when this is the account's very first login ever — lets the
    /// frontend skip the "welcome back" toast for a brand new account.
    pub is_first_login: bool,
    /// `true` when the account must choose a new password before using
    /// the platform (temporary password, or one below the current policy).
    /// Every endpoint except the password change itself answers
    /// `PASSWORD_CHANGE_REQUIRED` until then.
    pub must_change_password: bool,
}

/// Response of `POST /users/{id}/impersonate`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImpersonationResponse {
    /// A short-lived, read-only token acting as the target user.
    pub token: String,
    pub expires_at: chrono::NaiveDateTime,
    pub user_id: Uuid,
    pub username: String,
}
