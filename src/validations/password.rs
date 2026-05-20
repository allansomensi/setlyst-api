use std::borrow::Cow;
use validator::ValidationError;

pub fn validate_password(password: &str) -> Result<(), ValidationError> {
    if password.len() < 8 || password.len() > 100 {
        let mut error = ValidationError::new("invalid_password");
        error.message = Some(Cow::from("Password must be between 8 and 100 chars."));
        return Err(error);
    }

    Ok(())
}
