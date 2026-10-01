//! Platform-wide switches (`platform_settings.platform`): maintenance
//! mode, sign-ups and blocked e-mail domains.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::{Validate, ValidationError};

/// What maintenance mode stops. Staff (and "view as" sessions, which
/// are read-only anyway) are never affected, so they can check the
/// platform before reopening it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceMode {
    /// The platform is open.
    #[default]
    Off,
    /// Everyone can sign in and read; every change answers
    /// `MAINTENANCE_MODE` (503). For migrations and imports that must not
    /// race with edits.
    ReadOnly,
    /// Only staff can sign in or use the API; everyone else gets
    /// `MAINTENANCE_MODE` (503).
    Full,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct MaintenanceSettings {
    pub mode: MaintenanceMode,
    /// Shown to people while maintenance is on (any language; the clients
    /// show a translated default when empty).
    #[validate(length(max = 500))]
    pub message: Option<String>,
    /// When the platform is expected back (informational).
    pub ends_at: Option<NaiveDateTime>,
    /// When the current mode was switched on (set by the API).
    #[schema(read_only)]
    pub started_at: Option<NaiveDateTime>,
}

/// Every platform-wide switch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct PlatformSettings {
    #[validate(nested)]
    pub maintenance: MaintenanceSettings,
    /// While `false`, new accounts can't be created (password sign-up and
    /// first Google sign-in answer `REGISTRATION_CLOSED`). Staff can still
    /// create accounts from the console.
    pub registrations_open: bool,
    /// Domains (and their subdomains) new addresses can't use: sign-up,
    /// Google sign-up and e-mail changes answer `EMAIL_DOMAIN_BLOCKED`.
    /// For disposable-mail services and abuse waves. Existing accounts
    /// keep their addresses.
    #[validate(length(max = 500), custom(function = "validate_domains"))]
    pub blocked_email_domains: Vec<String>,
}

impl Default for PlatformSettings {
    fn default() -> Self {
        Self {
            maintenance: MaintenanceSettings::default(),
            registrations_open: true,
            blocked_email_domains: Vec::new(),
        }
    }
}

fn validate_domains(domains: &[String]) -> Result<(), ValidationError> {
    for domain in domains {
        if normalize_domain(domain).is_none() {
            let mut error = ValidationError::new("domain");
            error.message = Some(format!("'{domain}' is not a valid domain.").into());
            return Err(error);
        }
    }
    Ok(())
}

/// `domain` in the ASCII form mail actually uses (UTS 46: lower case,
/// full-width letters and ideographic dots folded, internationalized
/// names as punycode), without a trailing dot. `None` when it can't be
/// mapped. Addresses pass validation in any of these forms, so blocked
/// domains are compared in this one.
pub fn ascii_domain(domain: &str) -> Option<String> {
    let ascii = idna::domain_to_ascii(domain.trim()).ok()?;
    let ascii = ascii.trim_end_matches('.');
    (!ascii.is_empty()).then(|| ascii.to_string())
}

/// `domain` in ASCII form ([`ascii_domain`]), without a leading `@` or
/// `.`, or `None` when it isn't a plausible host name (`example.com`,
/// `mail.example.co.uk`, `bücher.de` stored as `xn--bcher-kva.de`).
pub fn normalize_domain(domain: &str) -> Option<String> {
    let domain = ascii_domain(
        domain
            .trim()
            .trim_start_matches('@')
            .trim_start_matches('.'),
    )?;
    let valid = domain.len() <= 253
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    valid.then_some(domain)
}

/// `PUT /admin/settings/platform`: every switch, spelled out. Unlike the
/// stored [`PlatformSettings`] (which fills in what an older version
/// didn't store), nothing defaults here: a body that leaves a switch out
/// (or misspells it) would otherwise reopen sign-ups or unblock every
/// domain without anyone asking.
#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlatformSettings {
    #[validate(nested)]
    pub maintenance: UpdateMaintenanceSettings,
    pub registrations_open: bool,
    #[validate(length(max = 500), custom(function = "validate_domains"))]
    pub blocked_email_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct UpdateMaintenanceSettings {
    pub mode: MaintenanceMode,
    #[serde(default)]
    #[validate(length(max = 500))]
    pub message: Option<String>,
    #[serde(default)]
    pub ends_at: Option<NaiveDateTime>,
    /// Ignored: set by the API (accepted so clients can send back what
    /// they read).
    #[serde(default)]
    pub started_at: Option<NaiveDateTime>,
}

impl UpdatePlatformSettings {
    pub fn into_settings(self) -> PlatformSettings {
        PlatformSettings {
            maintenance: MaintenanceSettings {
                mode: self.maintenance.mode,
                message: self.maintenance.message,
                ends_at: self.maintenance.ends_at,
                started_at: None,
            },
            registrations_open: self.registrations_open,
            blocked_email_domains: self.blocked_email_domains,
        }
    }
}

impl PlatformSettings {
    /// The settings as they are stored: domains normalized, sorted and
    /// deduplicated; a blank message dropped.
    pub fn normalized(mut self) -> Self {
        let mut domains: Vec<String> = self
            .blocked_email_domains
            .iter()
            .filter_map(|d| normalize_domain(d))
            .collect();
        domains.sort();
        domains.dedup();
        self.blocked_email_domains = domains;
        self.maintenance.message = self
            .maintenance
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        if self.maintenance.mode == MaintenanceMode::Off {
            self.maintenance.started_at = None;
        }
        self
    }

    /// `true` when `email`'s domain (or a parent of it) is blocked.
    pub fn is_email_blocked(&self, email: &str) -> bool {
        let Some((_, domain)) = email.trim().rsplit_once('@') else {
            return false;
        };
        let domain =
            ascii_domain(domain).unwrap_or_else(|| domain.trim_end_matches('.').to_lowercase());
        self.blocked_email_domains.iter().any(|blocked| {
            domain == *blocked
                || domain
                    .strip_suffix(blocked.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        })
    }

    /// What anyone may know about the platform's state.
    pub fn public(&self) -> PublicPlatformStatus {
        PublicPlatformStatus {
            maintenance: PublicMaintenance {
                mode: self.maintenance.mode,
                message: self.maintenance.message.clone(),
                ends_at: self.maintenance.ends_at,
                started_at: self.maintenance.started_at,
            },
            registrations_open: self.registrations_open,
        }
    }
}

/// `GET /public/platform`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicPlatformStatus {
    pub maintenance: PublicMaintenance,
    pub registrations_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicMaintenance {
    pub mode: MaintenanceMode,
    pub message: Option<String>,
    pub ends_at: Option<NaiveDateTime>,
    pub started_at: Option<NaiveDateTime>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_normalized_and_matched_with_subdomains() {
        let settings = PlatformSettings {
            blocked_email_domains: vec![
                " @Mailinator.com ".into(),
                "tempmail.io".into(),
                "tempmail.io".into(),
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(
            settings.blocked_email_domains,
            vec!["mailinator.com".to_string(), "tempmail.io".to_string()]
        );
        assert!(settings.is_email_blocked("a@mailinator.com"));
        assert!(settings.is_email_blocked("a@EU.Mailinator.com"));
        assert!(!settings.is_email_blocked("a@notmailinator.com"));
        assert!(!settings.is_email_blocked("a@gmail.com"));
        assert!(!settings.is_email_blocked("no-at-sign"));
        // Other spellings of the same domain that pass address validation.
        assert!(settings.is_email_blocked("a@ｍａｉｌｉｎａｔｏｒ.com"));
        assert!(settings.is_email_blocked("a@mailinator\u{3002}com"));
        assert!(settings.is_email_blocked("a@mailinator.com."));
    }

    #[test]
    fn internationalized_domains_are_blocked_in_any_form() {
        let settings = PlatformSettings {
            blocked_email_domains: vec!["Bücher.de".into()],
            ..Default::default()
        }
        .normalized();
        assert_eq!(settings.blocked_email_domains, vec!["xn--bcher-kva.de"]);
        assert!(settings.is_email_blocked("a@bücher.de"));
        assert!(settings.is_email_blocked("a@xn--bcher-kva.de"));
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn a_settings_update_must_name_every_switch() {
        // A partial body must not silently reset the switches it leaves out.
        for body in [
            r#"{"maintenance":{"mode":"full"}}"#,
            r#"{"maintenance":{"mode":"off"},"registration_open":false,"blocked_email_domains":[]}"#,
            r#"{"maintenance":{},"registrations_open":true,"blocked_email_domains":[]}"#,
        ] {
            assert!(
                serde_json::from_str::<UpdatePlatformSettings>(body).is_err(),
                "{body}"
            );
        }
        let full: UpdatePlatformSettings = serde_json::from_str(
            r#"{"maintenance":{"mode":"read_only","message":null,"ends_at":null,"started_at":null},
                "registrations_open":false,"blocked_email_domains":["x.com"]}"#,
        )
        .unwrap();
        let settings = full.into_settings();
        assert_eq!(settings.maintenance.mode, MaintenanceMode::ReadOnly);
        assert!(!settings.registrations_open);
    }

    #[test]
    fn invalid_domains_are_refused() {
        for bad in ["localhost", "a..b", "-x.com", "exa mple.com", ""] {
            assert!(normalize_domain(bad).is_none(), "{bad}");
        }
        let settings = PlatformSettings {
            blocked_email_domains: vec!["not a domain".into()],
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }
}
