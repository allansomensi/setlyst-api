use std::borrow::Cow;
use validator::ValidationError;

/// Longest first/last name accepted — matches the `VARCHAR(50)` columns.
pub const MAX_NAME_LENGTH: usize = 50;

fn validate_person_name(
    value: &str,
    code: &'static str,
    label: &str,
) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    let length = trimmed.chars().count();

    // Lengths are counted in characters, not bytes: "João" is 4 characters
    // but 5 bytes, and a byte-based limit silently rejected short
    // accented names.
    if length == 0 || length > MAX_NAME_LENGTH {
        let mut error = ValidationError::new(code);
        error.message = Some(Cow::from(format!(
            "{label} must be between 1 and {MAX_NAME_LENGTH} characters."
        )));
        return Err(error);
    }

    if !trimmed
        .chars()
        .all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '\'' || c == '.')
    {
        let mut error = ValidationError::new(code);
        error.message = Some(Cow::from(format!(
            "{label} can only contain letters, spaces, hyphens, apostrophes and periods."
        )));
        return Err(error);
    }

    Ok(())
}

pub fn validate_first_name(first_name: &str) -> Result<(), ValidationError> {
    validate_person_name(first_name, "invalid_first_name", "First name")
}

pub fn validate_last_name(last_name: &str) -> Result<(), ValidationError> {
    validate_person_name(last_name, "invalid_last_name", "Last name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_short_and_accented_names() {
        for name in ["Li", "Bo", "João", "Émile", "O'Brien", "Anne-Marie", "Jr."] {
            assert!(validate_first_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_empty_long_or_symbolic_names() {
        assert!(validate_first_name("   ").is_err());
        assert!(validate_last_name(&"a".repeat(51)).is_err());
        assert!(validate_last_name("Silva<script>").is_err());
        assert!(validate_first_name("R2D2").is_err());
    }
}
