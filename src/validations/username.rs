//! Username rules.
//!
//! Usernames appear in URLs, mentions, band rosters and audit logs, so
//! they're restricted to a conservative, unambiguous character set — the
//! same shape most platforms use:
//!
//! - 3 to 20 characters;
//! - ASCII letters, digits, `.`, `_` and `-` only;
//! - must start with a letter and end with a letter or digit;
//! - no two separators in a row (`a..b`, `a_-b`);
//! - not a reserved word that could be mistaken for staff or a system page.
//!
//! Uniqueness is case-insensitive and enforced separately (see
//! `UserRepository::is_unique`). These rules apply whenever a username is
//! *set*; existing accounts created under older rules keep working.

use std::borrow::Cow;
use validator::ValidationError;

pub const MIN_USERNAME_LENGTH: usize = 3;
pub const MAX_USERNAME_LENGTH: usize = 20;

const SEPARATORS: [char; 3] = ['.', '_', '-'];

/// Names that would let an account pass itself off as staff, a system
/// account or an app route. Compared case-insensitively, ignoring
/// separators ("ad.min" is still "admin").
const RESERVED: &[&str] = &[
    "admin",
    "administrator",
    "administrador",
    "root",
    "system",
    "sistema",
    "support",
    "suporte",
    "soporte",
    "help",
    "ajuda",
    "ayuda",
    "moderator",
    "moderador",
    "mod",
    "staff",
    "official",
    "oficial",
    "team",
    "equipe",
    "security",
    "seguranca",
    "api",
    "www",
    "mail",
    "null",
    "undefined",
    "anonymous",
    "me",
    "settings",
    "login",
    "logout",
    "register",
    "signup",
    "dashboard",
    "status",
    "about",
    "legal",
    "privacy",
    "terms",
    "wiki",
    "setlyst",
];

fn error(message: &'static str) -> ValidationError {
    let mut error = ValidationError::new("invalid_username");
    error.message = Some(Cow::from(message));
    error
}

pub fn validate_username(username: &str) -> Result<(), ValidationError> {
    let length = username.chars().count();
    if !(MIN_USERNAME_LENGTH..=MAX_USERNAME_LENGTH).contains(&length) {
        return Err(error("Username must be between 3 and 20 characters."));
    }

    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || SEPARATORS.contains(&c))
    {
        return Err(error(
            "Username can only contain letters (a-z), numbers, dots, underscores and hyphens.",
        ));
    }

    let first = username.chars().next().unwrap_or_default();
    if !first.is_ascii_alphabetic() {
        return Err(error("Username must start with a letter."));
    }

    let last = username.chars().last().unwrap_or_default();
    if !last.is_ascii_alphanumeric() {
        return Err(error("Username must end with a letter or a number."));
    }

    let mut previous_was_separator = false;
    for c in username.chars() {
        let is_separator = SEPARATORS.contains(&c);
        if is_separator && previous_was_separator {
            return Err(error(
                "Username cannot contain two special characters in a row.",
            ));
        }
        previous_was_separator = is_separator;
    }

    let normalized: String = username
        .chars()
        .filter(|c| !SEPARATORS.contains(c))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if RESERVED.contains(&normalized.as_str()) || normalized.starts_with("setlyst") {
        return Err(error(
            "This username is reserved. Please choose another one.",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_shapes() {
        for name in [
            "augusto",
            "Allan_Somensi",
            "joao.silva",
            "dj-kiko",
            "user42",
            "abc",
        ] {
            assert!(validate_username(name).is_ok(), "{name} should be valid");
        }
    }

    #[test]
    fn enforces_length_in_characters() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username(&"a".repeat(21)).is_err());
        assert!(validate_username(&"a".repeat(20)).is_ok());
    }

    #[test]
    fn rejects_unsafe_or_confusing_characters() {
        for name in ["joão", "a b c", "user@home", "<script>", "émile", "a/b"] {
            assert!(validate_username(name).is_err(), "{name} should be invalid");
        }
    }

    #[test]
    fn enforces_start_end_and_separator_rules() {
        assert!(validate_username("_augusto").is_err());
        assert!(validate_username("1augusto").is_err());
        assert!(validate_username("augusto.").is_err());
        assert!(validate_username("au..gusto").is_err());
        assert!(validate_username("au_-gusto").is_err());
    }

    #[test]
    fn rejects_reserved_names_regardless_of_case_or_separators() {
        for name in [
            "admin",
            "ADMIN",
            "ad.min",
            "Support",
            "setlyst",
            "setlyst_team",
            "me1",
        ] {
            let result = validate_username(name);
            if name == "me1" {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err(), "{name} should be reserved");
            }
        }
    }
}
