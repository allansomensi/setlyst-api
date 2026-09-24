use chrono::NaiveDateTime;
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
    /// Username or e-mail address (case-insensitive).
    #[validate(length(min = 1, max = 254, message = "Username is required."))]
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
    /// Always `false` here: when a second factor is needed the sign-in
    /// answers with a [`TwoFactorChallengeResponse`] instead.
    pub two_factor_required: bool,
    /// `true` when the account accepted the terms currently in force.
    /// Otherwise the client asks for consent (`POST /users/me/accept-terms`).
    pub terms_accepted: bool,
    pub email_verified: bool,
    /// `true` when this sign-in created the account (Google sign-in).
    pub is_new_account: bool,
}

/// First step of a sign-in to an account with two-factor authentication:
/// the credentials were right, and `challenge_token` must now be sent to
/// `POST /auth/login/2fa` together with a code.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorChallengeResponse {
    /// Always `true`.
    pub two_factor_required: bool,
    pub challenge_token: String,
    pub challenge_expires_at: NaiveDateTime,
}

/// What a sign-in answers: a session, or a second-factor challenge.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum LoginOutcome {
    Session(LoginResponse),
    TwoFactor(TwoFactorChallengeResponse),
}

/// Second step of a sign-in: exactly one of `code` (authenticator app) or
/// `recovery_code`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct LoginTwoFactorPayload {
    #[validate(length(min = 1, max = 128))]
    pub challenge_token: String,
    #[validate(length(min = 6, max = 8))]
    pub code: Option<String>,
    #[validate(length(min = 8, max = 16))]
    pub recovery_code: Option<String>,
}

/// Body of `POST /auth/password/forgot`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ForgotPasswordPayload {
    /// Username or e-mail address.
    #[validate(length(min = 1, max = 254))]
    pub identifier: String,
}

/// Body of `POST /auth/password/reset`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ResetPasswordPayload {
    #[validate(length(min = 1, max = 254))]
    pub identifier: String,
    #[validate(length(min = 6, max = 6, message = "The code has 6 digits."))]
    pub code: String,
    /// Checked against the password policy only *after* the code: an
    /// earlier `WEAK_PASSWORD` would tell whether the identifier exists.
    #[validate(length(max = 1024))]
    pub new_password: String,
}

/// Body of `POST /auth/oauth/google`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct GoogleSignInPayload {
    /// The ID token (JWT) obtained from Google Identity Services.
    #[validate(length(min = 20, max = 8192))]
    pub id_token: String,
    #[validate(length(max = 32))]
    pub referral_code: Option<String>,
    /// Required (`true`) to create a new account.
    #[serde(default)]
    pub accept_terms: bool,
    /// Required (`true`) to create a new account
    /// (`AGE_CONFIRMATION_REQUIRED`): the age declaration. Ignored for
    /// existing accounts.
    #[serde(default)]
    pub age_confirmed: bool,
    #[serde(default)]
    pub marketing_opt_in: bool,
    #[validate(length(max = 10))]
    pub locale: Option<String>,
}

/// Answer of the password reset: every session was revoked.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReauthRequiredResponse {
    pub reauth_required: bool,
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
