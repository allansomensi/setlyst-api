#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to load environment variable: {0}")]
    EnvVarNotFound(#[from] std::env::VarError),

    #[error("Error loading .env file: {0}")]
    Dotenv(#[from] dotenvy::Error),

    #[error("Error parsing data: {0}")]
    ParsingError(#[from] std::io::Error),
}
