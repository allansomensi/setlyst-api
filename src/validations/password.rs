//! The platform's single password policy.
//!
//! Every path that sets a password (registration, admin-created accounts,
//! admin resets, self-service changes, the `create_superuser` binary) goes
//! through [`password_issues`], so no account can ever end up with a
//! password below the minimum bar. Accounts created before the policy was
//! tightened are caught at sign-in instead: a correct but non-compliant
//! password flags the account for a mandatory change (see
//! `controllers::auth::login`).
//!
//! The issue codes are part of the public API — the web client renders a
//! translated checklist from them — so never rename one.

use std::borrow::Cow;
use validator::ValidationError;

pub const MIN_PASSWORD_LENGTH: usize = 8;
pub const MAX_PASSWORD_LENGTH: usize = 128;

pub const ISSUE_TOO_SHORT: &str = "too_short";
pub const ISSUE_TOO_LONG: &str = "too_long";
pub const ISSUE_MISSING_LOWERCASE: &str = "missing_lowercase";
pub const ISSUE_MISSING_UPPERCASE: &str = "missing_uppercase";
pub const ISSUE_MISSING_DIGIT: &str = "missing_digit";
pub const ISSUE_MISSING_SYMBOL: &str = "missing_symbol";
pub const ISSUE_CONTAINS_USERNAME: &str = "contains_username";
pub const ISSUE_TOO_COMMON: &str = "too_common";

/// A small list of the most commonly breached passwords (and trivial
/// variants that would otherwise satisfy the character-class rules).
/// Compared case-insensitively.
const COMMON_PASSWORDS: &[&str] = &[
    "password",
    "password1",
    "password1!",
    "password123",
    "password123!",
    "passw0rd",
    "passw0rd!",
    "p@ssw0rd",
    "p@ssword1",
    "p@ssw0rd1",
    "p@ssw0rd123",
    "qwerty123",
    "qwerty123!",
    "qwerty@123",
    "abc12345",
    "abc123456",
    "abcd1234",
    "abc@1234",
    "12345678",
    "123456789",
    "1234567890",
    "11111111",
    "iloveyou",
    "iloveyou1",
    "welcome1",
    "welcome1!",
    "welcome123",
    "welcome@123",
    "admin123",
    "admin@123",
    "admin123!",
    "letmein1",
    "letmein1!",
    "changeme",
    "changeme1",
    "changeme1!",
    "senha123",
    "senha@123",
    "senha123!",
    "mudar123",
    "mudar@123",
    "setlyst123",
    "setlyst@123",
    "setlyst123!",
    "football1",
    "baseball1",
    "sunshine1",
    "princess1",
    "dragon123",
    "monkey123",
    "master123",
    "summer2024!",
    "summer2025!",
    "summer2026!",
    "winter2025!",
    "winter2026!",
    "brasil123",
    "brasil@123",
];

/// Every rule `password` breaks, in a stable order. Empty means compliant.
///
/// `username`, when known, is rejected as a substring of the password
/// (case-insensitively) — "augusto2024!" for the user "augusto" is the
/// first thing anyone would try.
pub fn password_issues(password: &str, username: Option<&str>) -> Vec<&'static str> {
    let mut issues = Vec::new();
    let length = password.chars().count();

    if length < MIN_PASSWORD_LENGTH {
        issues.push(ISSUE_TOO_SHORT);
    }
    if length > MAX_PASSWORD_LENGTH {
        issues.push(ISSUE_TOO_LONG);
    }
    if !password.chars().any(|c| c.is_lowercase()) {
        issues.push(ISSUE_MISSING_LOWERCASE);
    }
    if !password.chars().any(|c| c.is_uppercase()) {
        issues.push(ISSUE_MISSING_UPPERCASE);
    }
    if !password.chars().any(|c| c.is_numeric()) {
        issues.push(ISSUE_MISSING_DIGIT);
    }
    if !password
        .chars()
        .any(|c| !c.is_alphanumeric() && !c.is_whitespace())
    {
        issues.push(ISSUE_MISSING_SYMBOL);
    }

    if let Some(username) = username.map(str::trim).filter(|u| u.chars().count() >= 3)
        && password.to_lowercase().contains(&username.to_lowercase())
    {
        issues.push(ISSUE_CONTAINS_USERNAME);
    }

    let lowered = password.to_lowercase();
    if COMMON_PASSWORDS.contains(&lowered.as_str()) {
        issues.push(ISSUE_TOO_COMMON);
    }

    issues
}

/// `true` when `password` satisfies the whole policy.
pub fn is_password_compliant(password: &str, username: Option<&str>) -> bool {
    password_issues(password, username).is_empty()
}

/// `validator` adapter for payload fields. Cannot see the username, so
/// handlers that know it must additionally call [`password_issues`] with
/// it (see the call sites in `controllers::user` and `controllers::auth`).
pub fn validate_password(password: &str) -> Result<(), ValidationError> {
    let issues = password_issues(password, None);
    if issues.is_empty() {
        return Ok(());
    }

    let mut error = ValidationError::new("weak_password");
    error.message = Some(Cow::from(format!(
        "Password must be {MIN_PASSWORD_LENGTH}-{MAX_PASSWORD_LENGTH} characters and include an uppercase letter, a lowercase letter, a number and a symbol."
    )));
    for issue in issues {
        error.add_param(Cow::from(issue), &true);
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_strong_password() {
        assert!(password_issues("Gu1tar-Solo!", Some("augusto")).is_empty());
        assert!(is_password_compliant("Ação#2026x", None));
    }

    #[test]
    fn reports_every_missing_class() {
        let issues = password_issues("abc", None);
        assert!(issues.contains(&ISSUE_TOO_SHORT));
        assert!(issues.contains(&ISSUE_MISSING_UPPERCASE));
        assert!(issues.contains(&ISSUE_MISSING_DIGIT));
        assert!(issues.contains(&ISSUE_MISSING_SYMBOL));
        assert!(!issues.contains(&ISSUE_MISSING_LOWERCASE));
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 7 characters, 14 bytes — must still be "too short".
        assert!(password_issues("Çãõéí1!", None).contains(&ISSUE_TOO_SHORT));
    }

    #[test]
    fn rejects_overly_long_passwords() {
        let long = format!("Aa1!{}", "x".repeat(MAX_PASSWORD_LENGTH));
        assert!(password_issues(&long, None).contains(&ISSUE_TOO_LONG));
    }

    #[test]
    fn whitespace_is_not_a_symbol() {
        assert!(password_issues("Guitar Solo 1", None).contains(&ISSUE_MISSING_SYMBOL));
    }

    #[test]
    fn rejects_the_username_inside_the_password() {
        let issues = password_issues("Augusto#2026", Some("augusto"));
        assert_eq!(issues, vec![ISSUE_CONTAINS_USERNAME]);
    }

    #[test]
    fn ignores_very_short_usernames_for_the_substring_rule() {
        assert!(password_issues("Xy#12345ab", Some("ab")).is_empty());
    }

    #[test]
    fn rejects_common_passwords_even_when_they_satisfy_the_classes() {
        let issues = password_issues("P@ssw0rd1", None);
        assert_eq!(issues, vec![ISSUE_TOO_COMMON]);
    }

    #[test]
    fn validator_adapter_matches_the_policy() {
        assert!(validate_password("Str0ng!Pass").is_ok());
        let err = validate_password("weak").unwrap_err();
        assert_eq!(err.code, "weak_password");
        assert!(err.params.contains_key("too_short"));
    }
}
