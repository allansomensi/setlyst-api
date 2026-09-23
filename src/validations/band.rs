use std::borrow::Cow;
use validator::ValidationError;

pub const MIN_BAND_NAME_LENGTH: usize = 2;
pub const MAX_BAND_NAME_LENGTH: usize = 60;
pub const MAX_BAND_DESCRIPTION_LENGTH: usize = 1000;
pub const MAX_LOGO_URL_LENGTH: usize = 500;

fn error(code: &'static str, message: String) -> ValidationError {
    let mut error = ValidationError::new(code);
    error.message = Some(Cow::from(message));
    error
}

/// Band names are display text, not identifiers (the slug is derived
/// separately), so real-world names like "AC/DC", "Guns N' Roses" or
/// "Panic! at the Disco" must be accepted. Only characters that are never
/// part of a name — control characters and markup delimiters — are
/// rejected.
pub fn validate_band_name(name: &str) -> Result<(), ValidationError> {
    let length = name.trim().chars().count();
    if !(MIN_BAND_NAME_LENGTH..=MAX_BAND_NAME_LENGTH).contains(&length) {
        return Err(error(
            "invalid_band_name",
            format!(
                "Band name must be between {MIN_BAND_NAME_LENGTH} and {MAX_BAND_NAME_LENGTH} characters."
            ),
        ));
    }

    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>'))
    {
        return Err(error(
            "invalid_band_name",
            "Band name contains characters that are not allowed.".to_string(),
        ));
    }

    Ok(())
}

pub fn validate_band_description(description: &str) -> Result<(), ValidationError> {
    if description.chars().count() > MAX_BAND_DESCRIPTION_LENGTH {
        return Err(error(
            "invalid_band_description",
            format!("Description must be at most {MAX_BAND_DESCRIPTION_LENGTH} characters."),
        ));
    }
    Ok(())
}

/// Logo URLs end up in `<img src>` for every member, so only absolute
/// `https://` URLs are accepted — never `javascript:`, `data:` or plain
/// `http:` (mixed content). An empty string clears the logo.
pub fn validate_logo_url(url: &str) -> Result<(), ValidationError> {
    if url.is_empty() {
        return Ok(());
    }

    let valid = url.len() <= MAX_LOGO_URL_LENGTH
        && url.starts_with("https://")
        && url.len() > "https://".len()
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());

    if valid {
        Ok(())
    } else {
        Err(error(
            "invalid_logo_url",
            "Logo must be a valid https:// URL of at most 500 characters.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_band_names() {
        for name in [
            "AC/DC",
            "Guns N' Roses",
            "Panic! at the Disco",
            "Os Paralamas",
            "Mötley Crüe",
        ] {
            assert!(validate_band_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_bad_band_names() {
        assert!(validate_band_name("A").is_err());
        assert!(validate_band_name(&"x".repeat(61)).is_err());
        assert!(validate_band_name("<b>Band</b>").is_err());
        assert!(validate_band_name("Line\nBreak").is_err());
    }

    #[test]
    fn logo_urls_must_be_https() {
        assert!(validate_logo_url("").is_ok());
        assert!(validate_logo_url("https://cdn.example.com/logo.png").is_ok());
        assert!(validate_logo_url("http://example.com/logo.png").is_err());
        assert!(validate_logo_url("javascript:alert(1)").is_err());
        assert!(validate_logo_url("https://").is_err());
        assert!(validate_logo_url("https://a b").is_err());
    }
}
