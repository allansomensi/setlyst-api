use crate::{
    models::user_preferences::SUPPORTED_LANGUAGES,
    validations::{
        name::{validate_first_name, validate_last_name},
        password::validate_password,
        text::validate_reason,
        username::validate_username,
    },
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use std::borrow::Cow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidateEmail, ValidationError};

/// Version of the Terms of Use / Privacy Policy currently in force (shared
/// with the web's `LEGAL_VERSION`). Accounts whose `terms_version` differs
/// are asked to accept the new version.
pub const CURRENT_TERMS_VERSION: &str = "2026-09-24";

/// Longest accepted e-mail address (RFC 5321 path limit).
pub const MAX_EMAIL_LENGTH: usize = 254;
pub const MAX_BIO_LENGTH: usize = 280;
pub const MAX_LOCATION_LENGTH: usize = 80;
pub const MAX_INSTRUMENTS: usize = 8;
pub const MAX_INSTRUMENT_LENGTH: usize = 30;

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
    pub email_verified_at: Option<NaiveDateTime>,
    /// `false` for accounts created through Google that never chose a
    /// password (their hash is random and unusable).
    pub password_set: bool,
    /// Encrypted TOTP seed (see `utils::crypto`), set once 2FA is enabled.
    pub totp_secret_enc: Option<String>,
    pub totp_enabled_at: Option<NaiveDateTime>,
    pub totp_pending_secret_enc: Option<String>,
    pub totp_pending_created_at: Option<NaiveDateTime>,
    /// Last TOTP time step accepted, so a code can't be used twice.
    pub totp_last_step: Option<i64>,
    pub failed_login_count: i32,
    pub locked_until: Option<NaiveDateTime>,
    pub terms_accepted_at: Option<NaiveDateTime>,
    pub terms_version: Option<String>,
    pub referral_code: Option<String>,
    pub avatar_url: Option<String>,
    /// Last password change (sign-in challenges older than this are
    /// refused).
    pub password_changed_at: Option<NaiveDateTime>,
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
            email_verified_at: None,
            password_set: true,
            totp_secret_enc: None,
            totp_enabled_at: None,
            totp_pending_secret_enc: None,
            totp_pending_created_at: None,
            totp_last_step: None,
            failed_login_count: 0,
            locked_until: None,
            terms_accepted_at: None,
            terms_version: None,
            referral_code: None,
            avatar_url: None,
            password_changed_at: None,
        }
    }

    pub fn active_ban(&self) -> Option<Option<NaiveDateTime>> {
        active_ban(self.banned_at, self.banned_until, Utc::now().naive_utc())
    }

    pub fn two_factor_enabled(&self) -> bool {
        self.totp_enabled_at.is_some() && self.totp_secret_enc.is_some()
    }

    pub fn email_verified(&self) -> bool {
        self.email_verified_at.is_some() && self.email.is_some()
    }

    /// `true` when the account accepted the terms currently in force.
    pub fn terms_accepted(&self) -> bool {
        self.terms_version.as_deref() == Some(CURRENT_TERMS_VERSION)
    }

    /// The end of an active sign-in lockout, if any.
    pub fn locked_until_active(&self) -> Option<NaiveDateTime> {
        self.locked_until
            .filter(|until| *until > Utc::now().naive_utc())
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
    /// `true` once the owner proved they control `email`.
    pub email_verified: bool,
    /// Link to an externally hosted image. Clients load it through the
    /// web's image proxy, never directly.
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub instruments: Vec<String>,
    pub two_factor_enabled: bool,
    /// `false` for accounts created through Google that never chose a
    /// password (password recovery sets one).
    pub password_set: bool,
    /// The version of the terms the account last accepted.
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<NaiveDateTime>,
    pub referral_code: Option<String>,
}

impl UserPublic {
    /// `true` when the account accepted the terms currently in force.
    pub fn terms_accepted(&self) -> bool {
        self.terms_version.as_deref() == Some(CURRENT_TERMS_VERSION)
    }
}

/// A required e-mail address: syntactically valid and at most 254
/// characters.
pub fn validate_email_address(email: &str) -> Result<(), ValidationError> {
    let email = email.trim();
    if email.chars().count() <= MAX_EMAIL_LENGTH && email.validate_email() {
        return Ok(());
    }
    let mut error = ValidationError::new("email");
    error.message = Some(Cow::from("Invalid email address."));
    Err(error)
}

/// Canonical stored form of an e-mail address. Only surrounding blanks
/// are dropped: the local part is case-sensitive in theory, so the case
/// is kept and uniqueness is checked case-insensitively instead.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_string()
}

/// The mailbox an address really delivers to, for "one per person"
/// checks (trials): lower case, without a `+tag`, and for Gmail without
/// dots and with `googlemail.com` folded into `gmail.com`. Never stored
/// or used to send mail.
pub fn canonical_email(email: &str) -> String {
    let email = email.trim().to_lowercase();
    let Some((local, domain)) = email.rsplit_once('@') else {
        return email;
    };
    let local = local.split('+').next().unwrap_or(local);
    let (local, domain) = match domain {
        "gmail.com" | "googlemail.com" => (local.replace('.', ""), "gmail.com"),
        other => (local.to_string(), other),
    };
    format!("{local}@{domain}")
}

fn validate_optional_locale(locale: &str) -> Result<(), ValidationError> {
    if SUPPORTED_LANGUAGES.contains(&locale) {
        return Ok(());
    }
    let mut error = ValidationError::new("unsupported_language");
    error.message = Some(Cow::from("Unsupported language."));
    Err(error)
}

fn validate_referral_code(code: &str) -> Result<(), ValidationError> {
    let code = code.trim();
    if code.is_empty()
        || (code.len() <= 32 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return Ok(());
    }
    let mut error = ValidationError::new("referral_code");
    error.message = Some(Cow::from("Invalid referral code."));
    Err(error)
}

fn validate_bio(value: &str) -> Result<(), ValidationError> {
    if value.trim().chars().count() <= MAX_BIO_LENGTH {
        return Ok(());
    }
    let mut error = ValidationError::new("bio_too_long");
    error.message = Some(Cow::from(format!(
        "Bio must be at most {MAX_BIO_LENGTH} characters."
    )));
    Err(error)
}

fn validate_location(value: &str) -> Result<(), ValidationError> {
    if value.trim().chars().count() <= MAX_LOCATION_LENGTH {
        return Ok(());
    }
    let mut error = ValidationError::new("location_too_long");
    error.message = Some(Cow::from(format!(
        "Location must be at most {MAX_LOCATION_LENGTH} characters."
    )));
    Err(error)
}

fn validate_instruments(values: &[String]) -> Result<(), ValidationError> {
    let error = |message: String| {
        let mut error = ValidationError::new("invalid_instruments");
        error.message = Some(Cow::from(message));
        error
    };
    if values.len() > MAX_INSTRUMENTS {
        return Err(error(format!(
            "At most {MAX_INSTRUMENTS} instruments are allowed."
        )));
    }
    for value in values {
        let len = value.trim().chars().count();
        if len == 0 || len > MAX_INSTRUMENT_LENGTH {
            return Err(error(format!(
                "Each instrument must have between 1 and {MAX_INSTRUMENT_LENGTH} characters."
            )));
        }
        if value.chars().any(char::is_control) {
            return Err(error(
                "Instruments cannot contain control characters.".into(),
            ));
        }
    }
    Ok(())
}

/// Trims, drops blanks and removes case-insensitive duplicates (keeping
/// the first spelling), preserving order.
pub fn normalize_instruments(values: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .iter()
        .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|v| !v.is_empty())
        .filter(|v| seen.insert(v.to_lowercase()))
        .collect()
}

fn validate_optional_email(email: &str) -> Result<(), ValidationError> {
    if email.is_empty() || (email.len() <= MAX_EMAIL_LENGTH && email.validate_email()) {
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

/// Self-service sign-up.
#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct RegisterPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    /// Required (`EMAIL_REQUIRED` when missing): password recovery and
    /// security notices depend on it.
    #[validate(custom(function = "validate_email_address"))]
    pub email: Option<String>,

    #[validate(custom(function = "validate_password"))]
    #[serde(skip_serializing)]
    pub password: String,

    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,

    /// Must be `true` (`TERMS_NOT_ACCEPTED` otherwise): consent to the
    /// Terms of Use and Privacy Policy in force.
    #[serde(default)]
    pub accept_terms: bool,

    /// Opt-in to marketing e-mails. Off unless explicitly chosen.
    #[serde(default)]
    pub marketing_opt_in: bool,

    /// Must be `true` (`AGE_CONFIRMATION_REQUIRED` otherwise): the
    /// declaration of being 18 or older, or 16 or 17 with a guardian's
    /// authorization.
    #[serde(default)]
    pub age_confirmed: bool,

    /// The referral code of the account that invited this one.
    #[validate(custom(function = "validate_referral_code"))]
    pub referral_code: Option<String>,

    /// UI language (`en`, `pt-BR`, `es`), also used for e-mails.
    #[validate(custom(function = "validate_optional_locale"))]
    pub locale: Option<String>,
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

/// Self-service profile edit. Empty strings clear optional fields.
#[derive(Deserialize, Serialize, ToSchema, Validate, Default)]
pub struct UpdateCurrentUserPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: Option<String>,

    /// No longer accepted: answered with `BAD_REQUEST`. The address is
    /// changed through `POST /users/me/email/change`, which proves the
    /// owner controls the new one.
    #[schema(deprecated)]
    pub email: Option<String>,

    #[validate(custom(function = "validate_optional_first_name"))]
    pub first_name: Option<String>,

    #[validate(custom(function = "validate_optional_last_name"))]
    pub last_name: Option<String>,

    /// Up to 280 characters.
    #[validate(custom(function = "validate_bio"))]
    pub bio: Option<String>,

    /// Up to 80 characters.
    #[validate(custom(function = "validate_location"))]
    pub location: Option<String>,

    /// Up to 8 entries of 1 to 30 characters; trimmed and de-duplicated.
    #[validate(custom(function = "validate_instruments"))]
    pub instruments: Option<Vec<String>>,

    /// An `https` link to an image (see `validations::image_url`).
    pub avatar_url: Option<String>,
}

impl UpdateCurrentUserPayload {
    /// The account fields shared with the staff edit.
    pub fn account_fields(&self) -> UpdateUserPayload {
        UpdateUserPayload {
            username: self.username.clone(),
            email: None,
            first_name: self.first_name.clone(),
            last_name: self.last_name.clone(),
            role: None,
            status: None,
        }
    }

    pub fn has_profile_fields(&self) -> bool {
        self.bio.is_some()
            || self.location.is_some()
            || self.instruments.is_some()
            || self.avatar_url.is_some()
    }
}

/// Normalized public-profile changes (`None` = unchanged, `Some(None)` =
/// clear).
#[derive(Debug, Clone, Default)]
pub struct ProfileUpdate {
    pub bio: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub instruments: Option<Vec<String>>,
    pub avatar_url: Option<Option<String>>,
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

/// A band both the viewer and the profile owner belong to.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, FromRow)]
pub struct BandInCommon {
    pub id: Uuid,
    pub name: String,
    pub logo_url: Option<String>,
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
    /// `None` when there is no avatar (or staff removed it).
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub instruments: Vec<String>,
    pub bands_in_common: Vec<BandInCommon>,
    /// Same as `created_at`.
    pub member_since: NaiveDateTime,
    /// `true` when the caller is looking at their own profile.
    pub is_self: bool,
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
    /// Open moderation flags about this account.
    pub open_flags: i64,
}

impl UserPublic {
    /// Builds the profile view of `self` as seen by a caller who is (or
    /// isn't) staff. This is the one place that decides which fields a
    /// regular viewer never sees.
    pub fn into_profile_view(
        self,
        viewer_is_staff: bool,
        is_self: bool,
        bands_in_common: Vec<BandInCommon>,
        open_flags: i64,
    ) -> UserProfileView {
        let admin_details = viewer_is_staff.then_some(UserProfileAdminDetails {
            email: self.email,
            role: self.role,
            status: self.status,
            username_changed_at: self.username_changed_at,
            is_banned: self.is_banned,
            banned_until: self.banned_until,
            last_login_at: self.last_login_at,
            open_flags,
        });

        UserProfileView {
            id: self.id,
            username: self.username,
            first_name: self.first_name,
            last_name: self.last_name,
            created_at: self.created_at,
            avatar_url: self.avatar_url,
            bio: self.bio,
            location: self.location,
            instruments: self.instruments,
            bands_in_common,
            member_since: self.created_at,
            is_self,
            admin_details,
        }
    }
}

/// Body of `POST /users/me/accept-terms`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct AcceptTermsPayload {
    /// Must equal the version currently in force.
    #[validate(length(min = 1, max = 20))]
    pub version: String,
}

/// Body of `DELETE /users/me`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct DeleteAccountPayload {
    /// The current password (accounts with a password).
    #[validate(length(max = 256))]
    pub password: Option<String>,
    /// A code from `POST /users/me/reauth/code` (accounts without a
    /// password).
    #[validate(length(max = 16))]
    pub reauth_code: Option<String>,
    /// A current authenticator code or an unused recovery code, required
    /// when two-factor authentication is enabled.
    #[validate(length(max = 16))]
    pub code: Option<String>,
    /// The account's username, typed to confirm.
    #[validate(length(min = 1, max = 64))]
    pub confirmation: String,
}

/// Why a profile is being reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    InappropriateAvatar,
    OffensiveUsername,
    Impersonation,
    Spam,
    Other,
}

impl ReportReason {
    pub fn key(&self) -> &'static str {
        match self {
            ReportReason::InappropriateAvatar => "inappropriate_avatar",
            ReportReason::OffensiveUsername => "offensive_username",
            ReportReason::Impersonation => "impersonation",
            ReportReason::Spam => "spam",
            ReportReason::Other => "other",
        }
    }
}

/// Body of `POST /users/{id}/report`.
#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReportUserPayload {
    pub reason: ReportReason,
    #[validate(length(max = 500, message = "Details must be at most 500 characters."))]
    pub details: Option<String>,
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
    fn instruments_are_bounded_and_normalized() {
        assert!(validate_instruments(&["Guitar".into(), "Vocals".into()]).is_ok());
        assert!(validate_instruments(&vec!["x".to_string(); 9]).is_err());
        assert!(validate_instruments(&["".into()]).is_err());
        assert!(validate_instruments(&["a".repeat(31)]).is_err());
        assert_eq!(
            normalize_instruments(&[" Guitar ".into(), "guitar".into(), "Bass  guitar".into()]),
            vec!["Guitar".to_string(), "Bass guitar".to_string()]
        );
    }

    #[test]
    fn required_email_and_profile_bounds() {
        assert!(validate_email_address("ana@example.com").is_ok());
        assert!(validate_email_address("").is_err());
        assert!(validate_email_address(&format!("{}@example.com", "a".repeat(250))).is_err());
        assert!(validate_bio(&"a".repeat(280)).is_ok());
        assert!(validate_bio(&"a".repeat(281)).is_err());
        assert!(validate_location(&"a".repeat(81)).is_err());
        assert!(validate_referral_code("ABCD2345").is_ok());
        assert!(validate_referral_code("<script>").is_err());
    }

    #[test]
    fn canonical_emails_fold_tags_dots_and_case() {
        assert_eq!(
            canonical_email(" Ana.Maria+x@GMail.com "),
            "anamaria@gmail.com"
        );
        assert_eq!(canonical_email("a.n.a@googlemail.com"), "ana@gmail.com");
        assert_eq!(
            canonical_email("ana.m+promo@example.com"),
            "ana.m@example.com"
        );
        assert_eq!(canonical_email("not-an-email"), "not-an-email");
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
