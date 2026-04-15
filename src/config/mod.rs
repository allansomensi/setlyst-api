use crate::errors::config_error::ConfigError;
use std::sync::OnceLock;

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
}

static CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    pub fn init() -> Result<(), ConfigError> {
        environment::load_environment()?;
        Self::logger_init();

        let config = Config {
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0:8000".to_string()),
            database_url: std::env::var("DATABASE_URL")?,
            postgres_db: std::env::var("POSTGRES_DB")?,
            jwt_secret: std::env::var("JWT_SECRET")?,
            jwt_expiration_time: std::env::var("JWT_EXPIRATION_TIME")?
                .parse()
                .expect("Invalid JWT_EXPIRATION_TIME format"),
        };

        CONFIG.set(config).expect("Config already initialized");

        Ok(())
    }

    pub fn get() -> &'static Config {
        CONFIG.get().expect("Config is not initialized.")
    }
}
