use crate::errors::config_error::ConfigError;

mod cors;
mod environment;
mod logger;

pub struct Config {}

impl Config {
    pub fn init() -> Result<(), ConfigError> {
        environment::load_environment()?;
        Self::logger_init();
        Ok(())
    }
}
