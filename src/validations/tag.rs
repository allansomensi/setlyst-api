//! Song tags ("romântica", "balada", "abertura"...).
//!
//! Tags are free-form so every musician can build the vocabulary that fits
//! their repertoire, but they are normalized before storage so "Balada",
//! " balada " and "BALADA" are one tag, not three. Bounded in length and
//! count so a single song (or account) can't bloat the index.

use crate::errors::api_error::ApiError;

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

/// Normalizes, validates and de-duplicates a list of tags, preserving the
/// caller's order. Fails with a descriptive `BadRequest` on the first
/// invalid tag, or when there are more than [`MAX_TAGS_PER_SONG`].
pub fn normalize_tags(raw: &[String]) -> Result<Vec<String>, ApiError> {
    let mut tags: Vec<String> = Vec::new();

    for tag in raw {
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

        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }

    if tags.len() > MAX_TAGS_PER_SONG {
        return Err(ApiError::BadRequest(format!(
            "A song can have at most {MAX_TAGS_PER_SONG} tags."
        )));
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
    fn caps_the_number_of_tags() {
        let many: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
        assert!(normalize_tags(&many).is_err());
        assert_eq!(normalize_tags(&many[..10]).unwrap().len(), 10);
    }
}
