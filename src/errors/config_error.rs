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

    #[error("Invalid header value: {0}")]
    InvalidHeaderValue(#[from] axum::http::header::InvalidHeaderValue),
}
