use crate::errors::config_error::ConfigError;

pub fn load_environment() -> Result<(), ConfigError> {
    dotenvy::dotenv()?;
    Ok(())
}
