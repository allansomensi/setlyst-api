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
    pub database_max_connections: u32,
    /// Apply pending migrations at startup (default `true`).
    pub run_migrations: bool,
    pub jwt_secret: String,
    pub jwt_expiration_time: i64,
    /// Lifetime of read-only impersonation tokens, in seconds.
    pub impersonation_expiration_time: i64,
    pub cors_allowed_origins: Vec<HeaderValue>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError>
where
    ConfigError: From<T::Err>,
{
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().parse()?),
        _ => Ok(default),
    }
}

impl Config {
    pub fn init() -> Result<Option<WorkerGuard>, ConfigError> {
        environment::load_environment()?;
        let guard = Self::logger_init();

        let config = Self::from_env()?;
        CONFIG.set(config).expect("Config already initialized");

        Ok(guard)
    }

    /// Builds the configuration from environment variables, validating
    /// everything that would otherwise fail later at runtime.
    pub fn from_env() -> Result<Self, ConfigError> {
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

        let port = std::env::var("PORT").unwrap_or_else(|_| "8000".to_string());
        let host = format!("0.0.0.0:{port}");

        let run_migrations = std::env::var("RUN_MIGRATIONS")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        Ok(Config {
            host,
            database_url: std::env::var("DATABASE_URL")?,
            postgres_db: std::env::var("POSTGRES_DB")?,
            database_max_connections: env_or("DATABASE_MAX_CONNECTIONS", 10u32)?,
            run_migrations,
            jwt_secret,
            jwt_expiration_time: env_or("JWT_EXPIRATION_TIME", 86_400i64)?,
            impersonation_expiration_time: env_or("IMPERSONATION_EXPIRATION_TIME", 3_600i64)?,
            cors_allowed_origins,
        })
    }

    /// Installs `config` as the process-wide configuration if none is set
    /// yet. Used by integration tests, which build their own config
    /// instead of reading `.env`.
    pub fn init_with(config: Config) {
        let _ = CONFIG.set(config);
    }

    pub fn get() -> &'static Config {
        CONFIG.get().expect("Config is not initialized.")
    }
}
