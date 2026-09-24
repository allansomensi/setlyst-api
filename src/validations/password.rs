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
/// The password appears in a public breach corpus (Have I Been Pwned).
pub const ISSUE_BREACHED: &str = "breached";

/// How long the breach check may take before it is skipped.
const BREACH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const BREACH_RANGE_URL: &str = "https://api.pwnedpasswords.com/range/";

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

/// Whether the breach check runs: on unless `DISABLE_BREACHED_PASSWORD_CHECK`
/// is truthy, and never in test runs (`TEST_DATABASE_URL` set, or unit
/// tests), which must not reach the network.
fn breach_check_enabled() -> bool {
    if cfg!(test) {
        return false;
    }
    let disabled = std::env::var("DISABLE_BREACHED_PASSWORD_CHECK").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    });
    !disabled && std::env::var_os("TEST_DATABASE_URL").is_none()
}

/// `true` when a Pwned Passwords range answer (`SUFFIX:COUNT` lines)
/// lists `suffix` (upper-case hex, the SHA-1 minus its first 5 chars) with
/// a non-zero count (padding entries have a count of 0).
pub fn range_lists_suffix(body: &str, suffix: &str) -> bool {
    body.lines().any(|line| {
        let mut parts = line.trim().splitn(2, ':');
        matches!(
            (parts.next(), parts.next().map(|c| c.trim().parse::<u64>())),
            (Some(candidate), Some(Ok(count))) if count > 0 && candidate.eq_ignore_ascii_case(suffix)
        )
    })
}

/// Checks `password` against Have I Been Pwned with k-anonymity: only the
/// first 5 hex characters of its SHA-1 leave the server. Fails open: any
/// network problem (or more than 2 seconds) counts as "not breached", so
/// an outage never blocks sign-ups.
pub async fn is_breached(password: &str) -> bool {
    use sha1::{Digest, Sha1};
    use std::sync::LazyLock;

    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(BREACH_CHECK_TIMEOUT)
            .build()
            .unwrap_or_default()
    });

    if !breach_check_enabled() {
        return false;
    }
    let normalized = crate::utils::hashing::normalize_password(password);
    let digest = crate::utils::crypto::hex(&Sha1::digest(normalized.as_bytes())).to_uppercase();
    let (prefix, suffix) = digest.split_at(5);
    let request = CLIENT
        .get(format!("{BREACH_RANGE_URL}{prefix}"))
        .header("Add-Padding", "true")
        .send();
    let body = match tokio::time::timeout(BREACH_CHECK_TIMEOUT, async {
        request.await?.error_for_status()?.text().await
    })
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Breached-password check unavailable; skipped");
            return false;
        }
        Err(_) => {
            tracing::warn!("Breached-password check timed out; skipped");
            return false;
        }
    };
    range_lists_suffix(&body, suffix)
}

/// [`password_issues`] plus the breach check ([`is_breached`]), for the
/// places a password is chosen: sign-up, change and recovery. The breach
/// check only runs when every local rule passes.
pub async fn password_issues_checked(password: &str, username: Option<&str>) -> Vec<&'static str> {
    let mut issues = password_issues(password, username);
    if issues.is_empty() && is_breached(password).await {
        issues.push(ISSUE_BREACHED);
    }
    issues
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
    fn breach_ranges_are_parsed_with_padding() {
        let body =
            "0018A45C4D1DEF81644B54AB7F969B88D65:1\r\n00D4F6E8FA6EECAD2A3AA415EEC418D38EC:0\r\n";
        assert!(range_lists_suffix(
            body,
            "0018a45c4d1def81644b54ab7f969b88d65"
        ));
        assert!(!range_lists_suffix(
            body,
            "00D4F6E8FA6EECAD2A3AA415EEC418D38EC"
        ));
        assert!(!range_lists_suffix(body, "FFFF"));
        assert!(
            !breach_check_enabled(),
            "never reaches the network in tests"
        );
    }

    #[test]
    fn validator_adapter_matches_the_policy() {
        assert!(validate_password("Str0ng!Pass").is_ok());
        let err = validate_password("weak").unwrap_err();
        assert_eq!(err.code, "weak_password");
        assert!(err.params.contains_key("too_short"));
    }
}
