use std::borrow::Cow;
use validator::ValidationError;

pub fn validate_username(username: &str) -> Result<(), ValidationError> {
    if username.len() < 3 || username.len() > 20 {
        let mut error = ValidationError::new("invalid_username");
        error.message = Some(Cow::from("Username must be between 3 and 20 chars."));
        return Err(error);
    }

    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        let mut error = ValidationError::new("invalid_username");
        error.message = Some(Cow::from(
            "Username can only contain letters, numbers, underscores, and hyphens.",
        ));
        return Err(error);
    }

    Ok(())
}
