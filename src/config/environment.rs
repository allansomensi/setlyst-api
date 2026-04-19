use crate::errors::config_error::ConfigError;

pub fn load_environment() -> Result<(), ConfigError> {
    let _ = dotenvy::dotenv();

    Ok(())
}
