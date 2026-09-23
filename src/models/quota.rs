//! Resource quotas.
//!
//! The platform runs on modest infrastructure, so every kind of content a
//! user can create is capped. Limits come from platform-wide defaults
//! (editable by admins) and can be overridden per user, or lifted entirely
//! with the `unlimited` flag. Admins are always unlimited.
//!
//! Limits are grouped by what they count:
//!
//! - **per user** — the user's own personal content and memberships;
//! - **per band** — resolved from the band owner's quota, so an owner with
//!   a raised limit raises it for the bands they own;
//! - **per setlist** — items (songs + blocks + breaks) in a single setlist,
//!   resolved from the setlist's owner (or its band's owner).

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::{Validate, ValidationError};

/// Upper bound accepted for any limit, to keep typos (an extra zero or
/// three) from effectively disabling the protection.
pub const MAX_QUOTA_VALUE: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaResource {
    Songs,
    Artists,
    Setlists,
    Gigs,
    Tags,
    BandsOwned,
    BandMemberships,
    BandMembers,
    BandSetlists,
    BandGigs,
    BandSongs,
    SetlistItems,
}

impl QuotaResource {
    pub const ALL: [QuotaResource; 12] = [
        QuotaResource::Songs,
        QuotaResource::Artists,
        QuotaResource::Setlists,
        QuotaResource::Gigs,
        QuotaResource::Tags,
        QuotaResource::BandsOwned,
        QuotaResource::BandMemberships,
        QuotaResource::BandMembers,
        QuotaResource::BandSetlists,
        QuotaResource::BandGigs,
        QuotaResource::BandSongs,
        QuotaResource::SetlistItems,
    ];

    /// Resources counted against the user themselves (the rest are
    /// counted per band or per setlist).
    pub const PER_USER: [QuotaResource; 7] = [
        QuotaResource::Songs,
        QuotaResource::Artists,
        QuotaResource::Setlists,
        QuotaResource::Gigs,
        QuotaResource::Tags,
        QuotaResource::BandsOwned,
        QuotaResource::BandMemberships,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            QuotaResource::Songs => "songs",
            QuotaResource::Artists => "artists",
            QuotaResource::Setlists => "setlists",
            QuotaResource::Gigs => "gigs",
            QuotaResource::Tags => "tags",
            QuotaResource::BandsOwned => "bands_owned",
            QuotaResource::BandMemberships => "band_memberships",
            QuotaResource::BandMembers => "band_members",
            QuotaResource::BandSetlists => "band_setlists",
            QuotaResource::BandGigs => "band_gigs",
            QuotaResource::BandSongs => "band_songs",
            QuotaResource::SetlistItems => "setlist_items",
        }
    }
}

fn validate_limit(value: i64) -> Result<(), ValidationError> {
    if (0..=MAX_QUOTA_VALUE).contains(&value) {
        Ok(())
    } else {
        let mut error = ValidationError::new("invalid_quota");
        error.message = Some(std::borrow::Cow::from(format!(
            "Limits must be between 0 and {MAX_QUOTA_VALUE}."
        )));
        Err(error)
    }
}

/// A complete set of limits — the platform defaults, or the effective
/// limits for one user after applying their overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct QuotaLimits {
    #[validate(custom(function = "validate_limit"))]
    pub songs: i64,
    #[validate(custom(function = "validate_limit"))]
    pub artists: i64,
    #[validate(custom(function = "validate_limit"))]
    pub setlists: i64,
    #[validate(custom(function = "validate_limit"))]
    pub gigs: i64,
    #[validate(custom(function = "validate_limit"))]
    pub tags: i64,
    #[validate(custom(function = "validate_limit"))]
    pub bands_owned: i64,
    #[validate(custom(function = "validate_limit"))]
    pub band_memberships: i64,
    #[validate(custom(function = "validate_limit"))]
    pub band_members: i64,
    #[validate(custom(function = "validate_limit"))]
    pub band_setlists: i64,
    #[validate(custom(function = "validate_limit"))]
    pub band_gigs: i64,
    #[validate(custom(function = "validate_limit"))]
    pub band_songs: i64,
    #[validate(custom(function = "validate_limit"))]
    pub setlist_items: i64,
}

impl Default for QuotaLimits {
    /// Generous for a working musician or band, small enough that a
    /// single abusive account can't meaningfully load the database.
    fn default() -> Self {
        Self {
            songs: 1_000,
            artists: 500,
            setlists: 200,
            gigs: 500,
            tags: 100,
            bands_owned: 5,
            band_memberships: 20,
            band_members: 30,
            band_setlists: 300,
            band_gigs: 500,
            band_songs: 2_000,
            setlist_items: 150,
        }
    }
}

impl QuotaLimits {
    pub fn get(&self, resource: QuotaResource) -> i64 {
        match resource {
            QuotaResource::Songs => self.songs,
            QuotaResource::Artists => self.artists,
            QuotaResource::Setlists => self.setlists,
            QuotaResource::Gigs => self.gigs,
            QuotaResource::Tags => self.tags,
            QuotaResource::BandsOwned => self.bands_owned,
            QuotaResource::BandMemberships => self.band_memberships,
            QuotaResource::BandMembers => self.band_members,
            QuotaResource::BandSetlists => self.band_setlists,
            QuotaResource::BandGigs => self.band_gigs,
            QuotaResource::BandSongs => self.band_songs,
            QuotaResource::SetlistItems => self.setlist_items,
        }
    }

    /// Applies per-user overrides on top of `self`.
    pub fn with_overrides(mut self, overrides: &QuotaOverrides) -> Self {
        macro_rules! apply {
            ($($field:ident),*) => {
                $(if let Some(v) = overrides.$field { self.$field = v; })*
            };
        }
        apply!(
            songs,
            artists,
            setlists,
            gigs,
            tags,
            bands_owned,
            band_memberships,
            band_members,
            band_setlists,
            band_gigs,
            band_songs,
            setlist_items
        );
        self
    }
}

fn validate_optional_limit(value: i64) -> Result<(), ValidationError> {
    validate_limit(value)
}

/// Per-user overrides: `None` means "use the platform default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct QuotaOverrides {
    #[validate(custom(function = "validate_optional_limit"))]
    pub songs: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub artists: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub setlists: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub gigs: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub tags: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub bands_owned: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub band_memberships: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub band_members: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub band_setlists: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub band_gigs: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub band_songs: Option<i64>,
    #[validate(custom(function = "validate_optional_limit"))]
    pub setlist_items: Option<i64>,
}

/// A user's stored quota settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UserQuotaSettings {
    pub overrides: QuotaOverrides,
    pub unlimited: bool,
    pub updated_at: Option<NaiveDateTime>,
    pub updated_by_username: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateUserQuotaPayload {
    #[validate(nested)]
    pub overrides: QuotaOverrides,
    pub unlimited: bool,
}

/// One line of a usage report.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QuotaUsageItem {
    pub resource: QuotaResource,
    /// Current count. `None` for per-band/per-setlist limits, which have
    /// no single "used" value for a user.
    pub used: Option<i64>,
    /// Effective limit. `None` when the account is unlimited.
    pub limit: Option<i64>,
    /// Whether this limit comes from a per-user override.
    pub overridden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QuotaReport {
    pub unlimited: bool,
    pub items: Vec<QuotaUsageItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_apply_only_where_set() {
        let defaults = QuotaLimits::default();
        let effective = defaults.with_overrides(&QuotaOverrides {
            songs: Some(5),
            ..Default::default()
        });
        assert_eq!(effective.songs, 5);
        assert_eq!(effective.artists, defaults.artists);
    }

    #[test]
    fn every_resource_has_a_distinct_key() {
        let mut keys: Vec<_> = QuotaResource::ALL.iter().map(|r| r.key()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), QuotaResource::ALL.len());
    }

    #[test]
    fn limits_are_bounded() {
        let mut limits = QuotaLimits::default();
        assert!(limits.validate().is_ok());
        limits.songs = -1;
        assert!(limits.validate().is_err());
        let overrides = QuotaOverrides {
            gigs: Some(MAX_QUOTA_VALUE + 1),
            ..Default::default()
        };
        assert!(overrides.validate().is_err());
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let limits: QuotaLimits = serde_json::from_str(r#"{"songs": 3}"#).unwrap();
        assert_eq!(limits.songs, 3);
        assert_eq!(limits.gigs, QuotaLimits::default().gigs);
    }
}
