//! Song tags ("romântica", "balada", "abertura"...).
//!
//! Tags are free-form so every musician can build the vocabulary that fits
//! their repertoire, but they are normalized before storage so "Balada",
//! " balada " and "BALADA" are one tag, not three. Bounded in length and
//! count so a single song (or account) can't bloat the index.

use crate::errors::api_error::ApiError;
use std::collections::HashSet;

pub const MAX_TAG_LENGTH: usize = 30;
pub const MAX_TAGS_PER_SONG: usize = 10;

/// Lowercases, trims and collapses inner whitespace. Returns `None` for a
/// tag that is empty after normalization.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = collapsed.to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn is_allowed_tag_char(c: char) -> bool {
    c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' || c == '&' || c == '\''
}

fn too_many_tags() -> ApiError {
    ApiError::BadRequest(format!("A song can have at most {MAX_TAGS_PER_SONG} tags."))
}

/// Normalizes, validates and de-duplicates a list of tags, preserving the
/// caller's order. Fails with a descriptive `BadRequest` on the first
/// invalid tag, or when there are more than [`MAX_TAGS_PER_SONG`].
///
/// Oversized input is refused before any per-tag work: the list comes
/// straight from a request body, and even a linear pass over hundreds of
/// thousands of entries is CPU the async runtime can't spare. (Twice the
/// limit still leaves room for duplicates and blanks a form may send.)
pub fn normalize_tags(raw: &[String]) -> Result<Vec<String>, ApiError> {
    if raw.len() > MAX_TAGS_PER_SONG * 2 {
        return Err(too_many_tags());
    }

    let mut seen: HashSet<String> = HashSet::with_capacity(raw.len());
    let mut tags: Vec<String> = Vec::with_capacity(raw.len());

    for tag in raw {
        // Longer than any valid tag can be even after whitespace collapses
        // (a character is at most 4 bytes): reject without normalizing it.
        if tag.len() > MAX_TAG_LENGTH * 4 + 256 {
            return Err(ApiError::BadRequest(format!(
                "Tags must be at most {MAX_TAG_LENGTH} characters."
            )));
        }
        let Some(tag) = normalize_tag(tag) else {
            continue;
        };

        if tag.chars().count() > MAX_TAG_LENGTH {
            return Err(ApiError::BadRequest(format!(
                "Tags must be at most {MAX_TAG_LENGTH} characters."
            )));
        }

        if !tag.chars().all(is_allowed_tag_char) {
            return Err(ApiError::BadRequest(
                "Tags can only contain letters, numbers, spaces and - _ & ' characters."
                    .to_string(),
            ));
        }

        if seen.insert(tag.clone()) {
            tags.push(tag);
            if tags.len() > MAX_TAGS_PER_SONG {
                return Err(too_many_tags());
            }
        }
    }

    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalizes_and_deduplicates() {
        let result = normalize_tags(&tags(&[
            " Balada ",
            "BALADA",
            "Rock  n  Roll",
            "",
            "romântica",
        ]))
        .unwrap();
        assert_eq!(result, vec!["balada", "rock n roll", "romântica"]);
    }

    #[test]
    fn rejects_long_or_symbolic_tags() {
        assert!(normalize_tags(&tags(&[&"a".repeat(31)])).is_err());
        assert!(normalize_tags(&tags(&["<script>"])).is_err());
        assert!(normalize_tags(&tags(&["a,b"])).is_err());
    }

    #[test]
    fn oversized_lists_fail_before_any_work() {
        // A body-sized list is refused on its length alone.
        let flood: Vec<String> = (0..200_000).map(|i| format!("t{i}")).collect();
        let started = std::time::Instant::now();
        assert!(normalize_tags(&flood).is_err());
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        // Duplicates within the slack still normalize to one tag.
        let dupes: Vec<String> = (0..MAX_TAGS_PER_SONG * 2).map(|_| "x".into()).collect();
        assert_eq!(normalize_tags(&dupes).unwrap(), vec!["x"]);
        // A single absurdly long entry is refused without normalizing it.
        assert!(normalize_tags(&[" ".repeat(100_000)]).is_err());
    }

    #[test]
    fn caps_the_number_of_tags() {
        let many: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
        assert!(normalize_tags(&many).is_err());
        assert_eq!(normalize_tags(&many[..10]).unwrap().len(), 10);
    }
}
