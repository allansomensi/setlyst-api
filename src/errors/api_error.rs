use super::{
    auth_error,
    config_error::{self, ConfigError},
};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

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

    #[error("Incorrect password! Try again.")]
    WrongPassword,
}

#[derive(serde::Serialize)]
struct ErrorResponse {
    code: String,
    message: String,
    details: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status_code, error_response) = match &self {
            ApiError::DatabaseError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    code: String::from("DATABASE_ERROR"),
                    message: String::from("An unexpected database error occurred."),
                    details: Some(String::from("Please try again later or contact support.")),
                },
            ),
            ApiError::ValidationError(e) => (
                StatusCode::BAD_REQUEST,
                ErrorResponse {
                    code: String::from("VALIDATION_ERROR"),
                    message: String::from("One or more validation errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::EncryptionError(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    code: String::from("ENCRYPT_ERROR"),
                    message: String::from("One or more encryption errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::JWTError(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    code: String::from("JWT_ERROR"),
                    message: String::from("One or more JWT errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::ServerError(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    code: String::from("SERVER_ERROR"),
                    message: String::from("One or more server errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::AuthError(e) => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse {
                    code: String::from("AUTH_ERROR"),
                    message: String::from("One or more auth errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::ConfigError(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorResponse {
                    code: String::from("CONFIG_ERROR"),
                    message: String::from("One or more config errors occurred."),
                    details: Some(e.to_string()),
                },
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                ErrorResponse {
                    code: String::from("NOT_FOUND"),
                    message: String::from("The data provided does not exist."),
                    details: Some(String::from(
                        "Please check if the data is correct and try again.",
                    )),
                },
            ),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse {
                    code: String::from("UNAUTHORIZED"),
                    message: String::from("You are not allowed to continue."),
                    details: Some(String::from("Please try again later.")),
                },
            ),
            ApiError::WrongPassword => (
                StatusCode::UNAUTHORIZED,
                ErrorResponse {
                    code: String::from("WRONG_PASSWORD"),
                    message: String::from("Incorrect password! Try again."),
                    details: Some(String::from("Please try again.")),
                },
            ),
            ApiError::NotModified => (
                StatusCode::NOT_MODIFIED,
                ErrorResponse {
                    code: String::from("NOT_MODIFIED"),
                    message: String::from("No updates were made for the provided ID."),
                    details: Some(String::from(
                        "The provided ID may not exist, or no fields were changed. Please verify the ID and the update values.",
                    )),
                },
            ),
            ApiError::AlreadyExists => (
                StatusCode::CONFLICT,
                ErrorResponse {
                    code: String::from("ALREADY_EXISTS"),
                    message: String::from("A resource with the provided details already exists."),
                    details: Some(String::from("Please choose a different name.")),
                },
            ),
        };

        (status_code, Json(error_response)).into_response()
    }
}

impl From<std::env::VarError> for ApiError {
    fn from(e: std::env::VarError) -> ApiError {
        ApiError::ConfigError(ConfigError::EnvVarNotFound(e))
    }
}
