#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to load environment variable: {0}")]
    EnvVarNotFound(#[from] std::env::VarError),

    #[error("Error loading .env file: {0}")]
    Dotenv(#[from] dotenvy::Error),

    #[error("Error parsing data: {0}")]
    ParsingError(#[from] std::io::Error),

    #[error("Failed to parse integer: {0}")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("JWT_SECRET must be at least 32 characters long for security reasons")]
    InsecureJwtSecret,

    #[error(
        "DATA_ENCRYPTION_KEY is required: it encrypts two-factor secrets at rest. Generate one with `openssl rand -base64 32`, store it with your other secrets and never change it (stored secrets become unreadable). For local development only, ALLOW_DERIVED_DATA_KEY=true derives it from JWT_SECRET instead."
    )]
    MissingDataEncryptionKey,

    #[error("Invalid configuration value: {0}")]
    Invalid(String),

    #[error("Invalid header value: {0}")]
    InvalidHeaderValue(#[from] axum::http::header::InvalidHeaderValue),
}
