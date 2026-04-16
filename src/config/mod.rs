use crate::errors::config_error::ConfigError;
use axum::http::HeaderValue;
use std::sync::OnceLock;
use tracing_appender::non_blocking::WorkerGuard;

pub mod cors;
pub mod environment;
pub mod logger;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub database_url: String,
    pub postgres_db: String,
    pub jwt_secret: String,
    pub jwt_expiration_time: i64,
    pub cors_allowed_origins: Vec<HeaderValue>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    pub fn init() -> Result<WorkerGuard, ConfigError> {
        environment::load_environment()?;
        let guard = Self::logger_init();

        let jwt_secret = std::env::var("JWT_SECRET")?;
        if jwt_secret.len() < 32 {
            return Err(ConfigError::InsecureJwtSecret);
        }

        let cors_raw = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
        let mut cors_allowed_origins = Vec::new();

        for origin in cors_raw
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            cors_allowed_origins.push(origin.parse::<HeaderValue>()?);
        }

        let config = Config {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0:8000".to_string()),
            database_url: std::env::var("DATABASE_URL")?,
            postgres_db: std::env::var("POSTGRES_DB")?,
            jwt_secret,
            jwt_expiration_time: std::env::var("JWT_EXPIRATION_TIME")?.parse()?,
            cors_allowed_origins,
        };

        CONFIG.set(config).expect("Config already initialized");

        Ok(guard)
    }

    pub fn get() -> &'static Config {
        CONFIG.get().expect("Config is not initialized.")
    }
}
