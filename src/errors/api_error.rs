use super::{
    auth_error,
    config_error::{self, ConfigError},
};
use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
};
use chrono::NaiveDateTime;
use serde_json::{Value, json};
use thiserror::Error;
use tracing::error;

/// Stable, machine-readable error codes.
///
/// These are part of the public API contract: the frontend keys its
/// translated messages off them, so a code must never be renamed once
/// shipped. Add new ones instead.
pub mod codes {
    pub const INVALID_CREDENTIALS: &str = "INVALID_CREDENTIALS";
    pub const ACCOUNT_BANNED: &str = "ACCOUNT_BANNED";
    pub const ACCOUNT_DEACTIVATED: &str = "ACCOUNT_DEACTIVATED";
    pub const SESSION_REVOKED: &str = "SESSION_REVOKED";
    pub const PASSWORD_CHANGE_REQUIRED: &str = "PASSWORD_CHANGE_REQUIRED";
    pub const WEAK_PASSWORD: &str = "WEAK_PASSWORD";
    pub const PASSWORD_REUSED: &str = "PASSWORD_REUSED";
    pub const IMPERSONATION_READ_ONLY: &str = "IMPERSONATION_READ_ONLY";
    pub const QUOTA_EXCEEDED: &str = "QUOTA_EXCEEDED";
    pub const INSUFFICIENT_ROLE: &str = "INSUFFICIENT_ROLE";
    pub const CANNOT_TARGET_SELF: &str = "CANNOT_TARGET_SELF";
    pub const LAST_ADMIN: &str = "LAST_ADMIN";
    pub const SHARE_LOCKED: &str = "SHARE_LOCKED";
    pub const USERNAME_COOLDOWN: &str = "USERNAME_COOLDOWN";
    pub const USERNAME_TAKEN: &str = "USERNAME_TAKEN";
    pub const INVITE_INVALID: &str = "INVITE_INVALID";
    pub const ALREADY_MEMBER: &str = "ALREADY_MEMBER";
    pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";

    // Accounts and authentication.
    pub const TOO_MANY_ATTEMPTS: &str = "TOO_MANY_ATTEMPTS";
    pub const ACCOUNT_LOCKED: &str = "ACCOUNT_LOCKED";
    pub const INVALID_TWO_FACTOR_CODE: &str = "INVALID_TWO_FACTOR_CODE";
    pub const TWO_FACTOR_ALREADY_ENABLED: &str = "TWO_FACTOR_ALREADY_ENABLED";
    pub const TWO_FACTOR_NOT_ENABLED: &str = "TWO_FACTOR_NOT_ENABLED";
    pub const INVALID_CODE: &str = "INVALID_CODE";
    pub const CODE_EXPIRED: &str = "CODE_EXPIRED";
    pub const EMAIL_NOT_VERIFIED: &str = "EMAIL_NOT_VERIFIED";
    pub const EMAIL_TAKEN: &str = "EMAIL_TAKEN";
    pub const EMAIL_REQUIRED: &str = "EMAIL_REQUIRED";
    pub const EMAIL_ALREADY_VERIFIED: &str = "EMAIL_ALREADY_VERIFIED";
    pub const GOOGLE_SIGNIN_DISABLED: &str = "GOOGLE_SIGNIN_DISABLED";
    pub const INVALID_GOOGLE_TOKEN: &str = "INVALID_GOOGLE_TOKEN";
    pub const PASSWORD_NOT_SET: &str = "PASSWORD_NOT_SET";
    pub const TERMS_NOT_ACCEPTED: &str = "TERMS_NOT_ACCEPTED";
    pub const INVALID_IMAGE_URL: &str = "INVALID_IMAGE_URL";
    /// A stored two-factor secret can't be decrypted (the data encryption
    /// key changed). 500; details are only in the server log.
    pub const TWO_FACTOR_UNAVAILABLE: &str = "TWO_FACTOR_UNAVAILABLE";

    // Plans, promo codes and credits.
    pub const FEATURE_NOT_IN_PLAN: &str = "FEATURE_NOT_IN_PLAN";
    pub const PLAN_NOT_FOUND: &str = "PLAN_NOT_FOUND";
    pub const PROMO_CODE_INVALID: &str = "PROMO_CODE_INVALID";
    pub const PROMO_CODE_EXPIRED: &str = "PROMO_CODE_EXPIRED";
    pub const PROMO_CODE_EXHAUSTED: &str = "PROMO_CODE_EXHAUSTED";
    pub const PROMO_CODE_ALREADY_REDEEMED: &str = "PROMO_CODE_ALREADY_REDEEMED";
    pub const PROMO_CODE_NOT_ELIGIBLE: &str = "PROMO_CODE_NOT_ELIGIBLE";
    pub const INSUFFICIENT_CREDITS: &str = "INSUFFICIENT_CREDITS";
    pub const REWARD_NOT_FOUND: &str = "REWARD_NOT_FOUND";

    // Payments.
    pub const PAYMENTS_UNAVAILABLE: &str = "PAYMENTS_UNAVAILABLE";
    pub const BILLING_NOT_ENFORCED: &str = "BILLING_NOT_ENFORCED";
    pub const PLAN_NOT_PURCHASABLE: &str = "PLAN_NOT_PURCHASABLE";
    pub const PAID_SUBSCRIPTION_ACTIVE: &str = "PAID_SUBSCRIPTION_ACTIVE";
    pub const NO_PAID_SUBSCRIPTION: &str = "NO_PAID_SUBSCRIPTION";
    pub const PLAN_ALREADY_ACTIVE: &str = "PLAN_ALREADY_ACTIVE";
    pub const SUBSCRIPTION_PAST_DUE: &str = "SUBSCRIPTION_PAST_DUE";
    pub const SUBSCRIPTION_CANCELING: &str = "SUBSCRIPTION_CANCELING";
    pub const PAYMENT_DECLINED: &str = "PAYMENT_DECLINED";
    pub const PAYMENT_PROVIDER_ERROR: &str = "PAYMENT_PROVIDER_ERROR";

    // Content.
    pub const INVALID_LINK: &str = "INVALID_LINK";
    pub const REPERTOIRE_PROTECTED: &str = "REPERTOIRE_PROTECTED";
    pub const SONG_ALREADY_IN_SETLIST: &str = "SONG_ALREADY_IN_SETLIST";
    pub const SUGGESTION_CLOSED: &str = "SUGGESTION_CLOSED";
    pub const NOT_IN_TRASH: &str = "NOT_IN_TRASH";
    pub const RESTORE_CONFLICT: &str = "RESTORE_CONFLICT";
    pub const CHORDPRO_INVALID: &str = "CHORDPRO_INVALID";
    pub const CHORDPRO_TOO_LARGE: &str = "CHORDPRO_TOO_LARGE";

    // Moderation.
    /// The flag was already resolved (actioned or dismissed) by someone else.
    pub const FLAG_ALREADY_RESOLVED: &str = "FLAG_ALREADY_RESOLVED";

    // Capacity.
    pub const SERVICE_BUSY: &str = "SERVICE_BUSY";

    // Communications (platform, v0.12).
    /// An announcement that must stay visible (not dismissible, or waiting
    /// for an acknowledgement) was dismissed.
    pub const NOT_DISMISSIBLE: &str = "NOT_DISMISSIBLE";
    /// A published announcement can't be changed this way (only its text,
    /// button, end and display flags while active; nothing once ended or
    /// archived).
    pub const ANNOUNCEMENT_LOCKED: &str = "ANNOUNCEMENT_LOCKED";
}

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("An error occurred while connecting to the database: {0}")]
    DatabaseError(#[from] sqlx::Error),

    #[error("One or more validation errors occurred: {0}")]
    ValidationError(#[from] validator::ValidationErrors),

    #[error("One or more encryption errors occurred: {0}")]
    EncryptionError(#[from] argon2::password_hash::Error),

    #[error("One or more JWT errors occurred: {0}")]
    JWTError(#[from] jsonwebtoken::errors::Error),

    #[error("One or more server errors occurred: {0}")]
    ServerError(#[from] axum::Error),

    #[error("One or more auth errors occurred: {0}")]
    AuthError(#[from] auth_error::AuthError),

    #[error("One or more config errors occurred: {0}")]
    ConfigError(#[from] config_error::ConfigError),

    #[error("The provided data does not correspond to any existing resource.")]
    NotFound,

    #[error("A resource with the provided name already exists.")]
    AlreadyExists,

    #[error("No updates were made for the provided ID.")]
    NotModified,

    #[error("You are not allowed to continue.")]
    Unauthorized,

    #[error("You do not have permission to perform this action.")]
    Forbidden,

    #[error("{0}")]
    BadRequest(String),

    #[error("Incorrect password! Try again.")]
    WrongPassword,

    /// A business-rule failure with a stable [`codes`] entry, so clients can
    /// react to (and translate) it without parsing English prose.
    #[error("{message}")]
    Rule {
        status: StatusCode,
        code: &'static str,
        message: String,
        meta: Option<Value>,
    },
}

impl ApiError {
    pub fn rule(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Rule {
            status,
            code,
            message: message.into(),
            meta: None,
        }
    }

    pub fn rule_with_meta(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        meta: Value,
    ) -> Self {
        Self::Rule {
            status,
            code,
            message: message.into(),
            meta: Some(meta),
        }
    }

    pub fn invalid_credentials() -> Self {
        Self::rule(
            StatusCode::UNAUTHORIZED,
            codes::INVALID_CREDENTIALS,
            "Invalid username or password.",
        )
    }

    pub fn account_banned(until: Option<NaiveDateTime>, reason: Option<String>) -> Self {
        Self::rule_with_meta(
            StatusCode::FORBIDDEN,
            codes::ACCOUNT_BANNED,
            match until {
                Some(until) => format!(
                    "This account is suspended until {} UTC.",
                    until.format("%Y-%m-%d %H:%M")
                ),
                None => "This account is permanently suspended.".to_string(),
            },
            json!({ "until": until, "reason": reason }),
        )
    }

    pub fn account_deactivated() -> Self {
        Self::rule(
            StatusCode::FORBIDDEN,
            codes::ACCOUNT_DEACTIVATED,
            "This account is deactivated. Contact an administrator.",
        )
    }

    pub fn session_revoked() -> Self {
        Self::rule(
            StatusCode::UNAUTHORIZED,
            codes::SESSION_REVOKED,
            "Your session is no longer valid. Please sign in again.",
        )
    }

    pub fn password_change_required() -> Self {
        Self::rule(
            StatusCode::FORBIDDEN,
            codes::PASSWORD_CHANGE_REQUIRED,
            "You must change your password before continuing.",
        )
    }

    pub fn impersonation_read_only() -> Self {
        Self::rule(
            StatusCode::FORBIDDEN,
            codes::IMPERSONATION_READ_ONLY,
            "Changes are disabled while viewing the platform as another user.",
        )
    }

    pub fn insufficient_role(message: impl Into<String>) -> Self {
        Self::rule(StatusCode::FORBIDDEN, codes::INSUFFICIENT_ROLE, message)
    }

    pub fn cannot_target_self(message: impl Into<String>) -> Self {
        Self::rule(StatusCode::FORBIDDEN, codes::CANNOT_TARGET_SELF, message)
    }

    pub fn quota_exceeded(resource: &str, limit: i64) -> Self {
        Self::rule_with_meta(
            StatusCode::FORBIDDEN,
            codes::QUOTA_EXCEEDED,
            format!("You have reached the limit of {limit} for '{resource}'."),
            json!({ "resource": resource, "limit": limit }),
        )
    }

    pub fn weak_password(issues: &[&'static str]) -> Self {
        Self::rule_with_meta(
            StatusCode::BAD_REQUEST,
            codes::WEAK_PASSWORD,
            "The password does not meet the security requirements.",
            json!({ "issues": issues }),
        )
    }

    /// The machine-readable code this error serializes with.
    pub fn code(&self) -> &str {
        match self {
            ApiError::DatabaseError(e) if is_unique_violation(e) => "ALREADY_EXISTS",
            ApiError::DatabaseError(_) => "DATABASE_ERROR",
            ApiError::ValidationError(_) => "VALIDATION_ERROR",
            ApiError::EncryptionError(_) => "ENCRYPT_ERROR",
            ApiError::JWTError(_) => "JWT_ERROR",
            ApiError::ServerError(_) => "SERVER_ERROR",
            ApiError::AuthError(_) => "AUTH_ERROR",
            ApiError::ConfigError(_) => "CONFIG_ERROR",
            ApiError::NotFound => "NOT_FOUND",
            ApiError::AlreadyExists => "ALREADY_EXISTS",
            ApiError::NotModified => "UNPROCESSABLE_ENTITY",
            ApiError::Unauthorized => "UNAUTHORIZED",
            ApiError::Forbidden => "FORBIDDEN",
            ApiError::BadRequest(_) => "BAD_REQUEST",
            ApiError::WrongPassword => "WRONG_PASSWORD",
            ApiError::Rule { code, .. } => code,
        }
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    code: String,
    message: String,
    details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<Value>,
}

impl ErrorResponse {
    fn new(code: &str, message: impl Into<String>, details: Option<&str>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details: details.map(str::to_string),
            meta: None,
        }
    }
}

/// Flattens `validator` errors into `{ field: [{ code, message }] }` and
/// picks the first human-readable message, so a client can both show a
/// useful toast and highlight the offending fields.
fn describe_validation_errors(errors: &validator::ValidationErrors) -> (String, Value) {
    let mut fields = serde_json::Map::new();
    let mut first_message: Option<String> = None;

    let mut field_errors: Vec<_> = errors.field_errors().into_iter().collect();
    field_errors.sort_by(|a, b| a.0.cmp(&b.0));

    for (field, errs) in field_errors {
        let entries: Vec<Value> = errs
            .iter()
            .map(|err| {
                let message = err
                    .message
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| format!("Invalid value for '{field}'."));
                if first_message.is_none() {
                    first_message = Some(message.clone());
                }
                json!({ "code": err.code, "message": message })
            })
            .collect();
        fields.insert(field.to_string(), Value::Array(entries));
    }

    (
        first_message.unwrap_or_else(|| "One or more validation errors occurred.".to_string()),
        json!({ "fields": fields }),
    )
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.code().to_string();

        let (status_code, error_response) = match &self {
            ApiError::DatabaseError(e) if is_unique_violation(e) => (
                StatusCode::CONFLICT,
                ErrorResponse::new(
                    &code,
                    "A resource with the provided details already exists.",
                    Some("Please choose a different name."),
                ),
            ),
            ApiError::DatabaseError(e) => {
                error!(error = %e, "Unhandled database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse::new(
                        &code,
                        "An unexpected database error occurred.",
                        Some("Please try again later or contact support."),
                    ),
                )
            }
            ApiError::ValidationError(e) => {
                let (message, meta) = describe_validation_errors(e);
                let mut response = ErrorResponse::new(&code, message, None);
                response.meta = Some(meta);
                (StatusCode::BAD_REQUEST, response)
            }
            // Internal failures never echo their inner message back: it can
            // carry library internals that are meaningless (or sensitive)
            // to a client. The detail goes to the logs instead.
            ApiError::EncryptionError(e) => {
                error!(error = %e, "Encryption error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse::new(&code, "One or more encryption errors occurred.", None),
                )
            }
            ApiError::JWTError(e) => {
                error!(error = %e, "JWT error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse::new(&code, "One or more JWT errors occurred.", None),
                )
            }
            ApiError::ServerError(e) => {
                error!(error = %e, "Server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse::new(&code, "One or more server errors occurred.", None),
                )
            }
            ApiError::ConfigError(e) => {
                error!(error = %e, "Configuration error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorResponse::new(&code, "One or more config errors occurred.", None),
                )
            }
            ApiError::AuthError(e) => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse::new(&code, e.to_string(), None),
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                ErrorResponse::new(
                    &code,
                    "The data provided does not exist.",
                    Some("Please check if the data is correct and try again."),
                ),
            ),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse::new(
                    &code,
                    "You are not allowed to continue.",
                    Some("Please sign in again."),
                ),
            ),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                ErrorResponse::new(
                    &code,
                    "You do not have permission to perform this action.",
                    Some("Your current role does not grant access to this resource."),
                ),
            ),
            ApiError::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                ErrorResponse::new(&code, message.clone(), None),
            ),
            ApiError::WrongPassword => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse::new(
                    &code,
                    "Incorrect password! Try again.",
                    Some("Please try again."),
                ),
            ),
            ApiError::NotModified => (
                StatusCode::UNPROCESSABLE_ENTITY,
                ErrorResponse::new(
                    &code,
                    "No updates were made for the provided ID.",
                    Some("No fields were changed. Please verify the update values."),
                ),
            ),
            ApiError::AlreadyExists => (
                StatusCode::CONFLICT,
                ErrorResponse::new(
                    &code,
                    "A resource with the provided details already exists.",
                    Some("Please choose a different name."),
                ),
            ),
            ApiError::Rule {
                status,
                message,
                meta,
                ..
            } => {
                let mut response = ErrorResponse::new(&code, message.clone(), None);
                response.meta = meta.clone();
                (*status, response)
            }
        };

        let retry_after = retry_after_seconds(status_code, error_response.meta.as_ref());
        let mut response = (status_code, Json(error_response)).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

/// The `Retry-After` value (in whole seconds, at least 1) for a 429/503
/// answer whose meta says how long to wait: either
/// `meta.retry_after_seconds` or, for lockouts, `meta.until` (a UTC
/// timestamp, counted from now). `None` for any other answer, so clients
/// only back off when the server actually told them to.
fn retry_after_seconds(status: StatusCode, meta: Option<&Value>) -> Option<u64> {
    if status != StatusCode::TOO_MANY_REQUESTS && status != StatusCode::SERVICE_UNAVAILABLE {
        return None;
    }
    let meta = meta?;
    let seconds = match meta.get("retry_after_seconds") {
        Some(value) => value
            .as_i64()
            .or_else(|| value.as_f64().map(|secs| secs.ceil() as i64))?,
        None => {
            let until: NaiveDateTime = serde_json::from_value(meta.get("until")?.clone()).ok()?;
            let remaining = until - chrono::Utc::now().naive_utc();
            // Round up so a client never retries a moment too early.
            remaining.num_seconds() + i64::from(remaining.subsec_nanos() > 0)
        }
    };
    Some(seconds.max(1) as u64)
}

impl From<std::env::VarError> for ApiError {
    fn from(e: std::env::VarError) -> ApiError {
        ApiError::ConfigError(ConfigError::EnvVarNotFound(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_json(error: ApiError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn quota_errors_carry_structured_meta() {
        let (status, body) = body_json(ApiError::quota_exceeded("songs", 10)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "QUOTA_EXCEEDED");
        assert_eq!(body["meta"]["resource"], "songs");
        assert_eq!(body["meta"]["limit"], 10);
    }

    #[tokio::test]
    async fn waiting_rules_send_retry_after() {
        let busy = ApiError::rule_with_meta(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::SERVICE_BUSY,
            "Busy.",
            json!({ "retry_after_seconds": 10 }),
        )
        .into_response();
        assert_eq!(busy.headers()[RETRY_AFTER], "10");

        let attempts = crate::services::account::too_many_attempts(0).into_response();
        assert_eq!(attempts.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(attempts.headers()[RETRY_AFTER], "1");

        let until = chrono::Utc::now().naive_utc() + chrono::Duration::minutes(15);
        let locked = crate::services::account::account_locked(until).into_response();
        let seconds: u64 = locked.headers()[RETRY_AFTER]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((899..=900).contains(&seconds), "{seconds}");

        // A lock that just ended still asks for a (minimal) wait.
        let past = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1);
        let expired = crate::services::account::account_locked(past).into_response();
        assert_eq!(expired.headers()[RETRY_AFTER], "1");
    }

    #[tokio::test]
    async fn other_errors_do_not_send_retry_after() {
        // Same meta shape, but not a "wait" status.
        let rule = ApiError::rule_with_meta(
            StatusCode::BAD_REQUEST,
            codes::INVALID_CODE,
            "Nope.",
            json!({ "retry_after_seconds": 10 }),
        )
        .into_response();
        assert!(rule.headers().get(RETRY_AFTER).is_none());

        let disabled = ApiError::rule(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::GOOGLE_SIGNIN_DISABLED,
            "Off.",
        )
        .into_response();
        assert!(disabled.headers().get(RETRY_AFTER).is_none());
        assert!(
            ApiError::NotFound
                .into_response()
                .headers()
                .get(RETRY_AFTER)
                .is_none()
        );
    }

    #[tokio::test]
    async fn internal_errors_do_not_leak_details() {
        let (status, body) =
            body_json(ApiError::ServerError(axum::Error::new("secret internals"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!body.to_string().contains("secret internals"));
    }

    #[tokio::test]
    async fn validation_errors_surface_the_first_field_message() {
        use validator::Validate;

        #[derive(Validate)]
        struct Payload {
            #[validate(length(min = 3, message = "Too short."))]
            name: String,
        }

        let err = Payload {
            name: "a".to_string(),
        }
        .validate()
        .unwrap_err();

        let (status, body) = body_json(ApiError::from(err)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["message"], "Too short.");
        assert_eq!(body["meta"]["fields"]["name"][0]["code"], "length");
    }
}
