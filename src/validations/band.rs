use std::borrow::Cow;
use validator::ValidationError;

pub fn validate_band_name(name: &str) -> Result<(), ValidationError> {
    if name.trim().len() < 2 || name.len() > 60 {
        let mut error = ValidationError::new("invalid_band_name");
        error.message = Some(Cow::from("Band name must be between 2 and 60 chars."));
        return Err(error);
    }

    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || " -_&'.".contains(c))
    {
        let mut error = ValidationError::new("invalid_band_name");
        error.message = Some(Cow::from(
            "Band name can only contain letters, numbers, spaces, and -_&'. characters.",
        ));
        return Err(error);
    }

    Ok(())
}
