use std::borrow::Cow;
use validator::ValidationError;

pub fn validate_first_name(first_name: &str) -> Result<(), ValidationError> {
    if first_name.len() < 3 || first_name.len() > 20 {
        let mut error = ValidationError::new("invalid_first_name");
        error.message = Some(Cow::from("First name must be between 3 and 20 chars."));
        return Err(error);
    }

    if !first_name
        .chars()
        .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\'')
    {
        let mut error = ValidationError::new("invalid_first_name");
        error.message = Some(Cow::from(
            "First name can only contain letters, spaces, hyphens, and apostrophes.",
        ));
        return Err(error);
    }

    Ok(())
}

pub fn validate_last_name(last_name: &str) -> Result<(), ValidationError> {
    if last_name.len() < 2 || last_name.len() > 50 {
        let mut error = ValidationError::new("invalid_last_name");
        error.message = Some(Cow::from("Last name must be between 2 and 50 chars."));
        return Err(error);
    }

    if !last_name
        .chars()
        .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\'')
    {
        let mut error = ValidationError::new("invalid_last_name");
        error.message = Some(Cow::from(
            "Last name can only contain letters, spaces, hyphens, and apostrophes.",
        ));
        return Err(error);
    }

    Ok(())
}
