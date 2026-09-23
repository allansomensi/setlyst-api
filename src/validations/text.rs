//! Upper bounds for free-text fields.
//!
//! Every `TEXT` column that users can write to gets an explicit ceiling so
//! one request can't store megabytes in a single row (the request body
//! limit alone is far too generous for a field like a setlist
//! description). Counted in characters, not bytes.

use std::borrow::Cow;
use validator::ValidationError;

/// Lyrics/chord charts: generous enough for the longest real songs with
/// chords and annotations, small enough to keep rows and PDFs sane.
pub const MAX_LYRICS_LENGTH: usize = 50_000;
/// Setlist descriptions and gig notes.
pub const MAX_DESCRIPTION_LENGTH: usize = 2_000;
/// Moderation reasons (bans, share takedowns).
pub const MAX_REASON_LENGTH: usize = 500;

fn too_long(code: &'static str, max: usize) -> ValidationError {
    let mut error = ValidationError::new(code);
    error.message = Some(Cow::from(format!("Must be at most {max} characters.")));
    error
}

pub fn validate_lyrics(value: &str) -> Result<(), ValidationError> {
    if value.chars().count() > MAX_LYRICS_LENGTH {
        return Err(too_long("lyrics_too_long", MAX_LYRICS_LENGTH));
    }
    Ok(())
}

pub fn validate_description(value: &str) -> Result<(), ValidationError> {
    if value.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err(too_long("description_too_long", MAX_DESCRIPTION_LENGTH));
    }
    Ok(())
}

pub fn validate_reason(value: &str) -> Result<(), ValidationError> {
    if value.chars().count() > MAX_REASON_LENGTH {
        return Err(too_long("reason_too_long", MAX_REASON_LENGTH));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_counted_in_characters() {
        assert!(validate_description(&"é".repeat(MAX_DESCRIPTION_LENGTH)).is_ok());
        assert!(validate_description(&"é".repeat(MAX_DESCRIPTION_LENGTH + 1)).is_err());
        assert!(validate_lyrics(&"a".repeat(MAX_LYRICS_LENGTH + 1)).is_err());
        assert!(validate_reason("spam").is_ok());
    }
}
