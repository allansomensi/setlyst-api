//! Account security: e-mail verification and change, two-factor
//! authentication and linked sign-in providers.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};
use utoipa::ToSchema;
use validator::Validate;

use crate::models::user::validate_email_address;

/// What a one-time e-mail code is for (`verification_purpose` in SQL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "verification_purpose", rename_all = "snake_case")]
pub enum VerificationPurpose {
    EmailVerification,
    PasswordReset,
    EmailChange,
}

impl VerificationPurpose {
    pub fn key(&self) -> &'static str {
        match self {
            VerificationPurpose::EmailVerification => "email_verification",
            VerificationPurpose::PasswordReset => "password_reset",
            VerificationPurpose::EmailChange => "email_change",
        }
    }
}

/// A stored one-time code (the plain code only exists in the e-mail).
#[derive(Debug, Clone, FromRow)]
pub struct VerificationCode {
    pub id: uuid::Uuid,
    pub user_id: uuid::Uuid,
    pub purpose: VerificationPurpose,
    pub code_hash: String,
    pub target_email: String,
    pub attempts: i32,
    pub expires_at: NaiveDateTime,
    pub consumed_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

/// A pending second sign-in step.
#[derive(Debug, Clone, FromRow)]
pub struct LoginChallenge {
    pub id: uuid::Uuid,
    pub user_id: uuid::Uuid,
    pub method: String,
    pub attempts: i32,
    pub expires_at: NaiveDateTime,
    pub consumed_at: Option<NaiveDateTime>,
}

/// Answer of the endpoints that send a code by e-mail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeSentResponse {
    /// When the code stops working.
    pub expires_at: NaiveDateTime,
    /// Seconds before another code may be requested.
    pub resend_after_seconds: i64,
}

/// A 6-digit code received by e-mail.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct EmailCodePayload {
    #[validate(length(min = 6, max = 6, message = "The code has 6 digits."))]
    pub code: String,
}

/// Body of `POST /users/me/email/change`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct EmailChangePayload {
    #[validate(custom(function = "validate_email_address"))]
    pub new_email: String,
    /// Required when the account has a password.
    #[validate(length(max = 256))]
    pub password: Option<String>,
}

/// `GET /users/me/security`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SecurityOverview {
    pub two_factor_enabled: bool,
    pub two_factor_enabled_at: Option<NaiveDateTime>,
    pub recovery_codes_remaining: i64,
    pub password_set: bool,
    pub email_verified: bool,
    pub has_google: bool,
    pub last_login_at: Option<NaiveDateTime>,
}

/// Password confirmation for sensitive changes (required when the account
/// has a password).
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct PasswordConfirmationPayload {
    #[validate(length(max = 256))]
    pub password: Option<String>,
}

/// Answer of `POST /users/me/2fa/setup`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorSetupResponse {
    /// Base32 secret, for manual entry in the authenticator app.
    pub secret: String,
    /// The same secret as an `otpauth://` URI, rendered as a QR code.
    pub otpauth_url: String,
    /// The pending secret must be confirmed before this moment.
    pub expires_at: NaiveDateTime,
}

/// A code from the authenticator app (or, where accepted, a recovery code).
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct TwoFactorCodePayload {
    #[validate(length(min = 6, max = 16))]
    pub code: String,
}

/// Body of `POST /users/me/2fa/disable`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct TwoFactorDisablePayload {
    #[validate(length(max = 256))]
    pub password: Option<String>,
    /// A current code from the app, or an unused recovery code.
    #[validate(length(min = 6, max = 16))]
    pub code: String,
}

/// Freshly issued recovery codes. Shown once: only their hashes are kept.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecoveryCodesResponse {
    pub recovery_codes: Vec<String>,
}

/// A sign-in provider linked to the account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, FromRow)]
pub struct LinkedIdentity {
    /// `google`.
    pub provider: String,
    pub email: Option<String>,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
}
