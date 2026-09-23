//! Release notes ("What's new"), editable from the staff console.

use crate::models::billing::validate_localized_map;
use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use std::borrow::Cow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidationError};

pub const ITEM_KINDS: [&str; 4] = ["new", "improved", "fixed", "security"];

/// One line of a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseNoteItem {
    /// `new`, `improved`, `fixed` or `security`.
    pub kind: String,
    /// Localized text map (`en` and `pt-BR` required, `es` optional).
    pub text: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct ReleaseNoteRow {
    pub id: Uuid,
    pub version: String,
    pub title: Value,
    pub items: Value,
    pub released_on: NaiveDate,
    pub published_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub updated_by_username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReleaseNote {
    pub id: Uuid,
    pub version: String,
    /// Localized title map.
    pub title: Value,
    pub items: Vec<ReleaseNoteItem>,
    pub released_on: NaiveDate,
    /// `None` = draft.
    pub published_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub updated_by_username: Option<String>,
    /// `true` when edited more than a minute after publication.
    pub is_edited: bool,
}

impl From<ReleaseNoteRow> for ReleaseNote {
    fn from(row: ReleaseNoteRow) -> Self {
        let is_edited = row
            .published_at
            .is_some_and(|published| row.updated_at > published + Duration::minutes(1));
        Self {
            id: row.id,
            version: row.version,
            title: row.title,
            items: serde_json::from_value(row.items).unwrap_or_default(),
            released_on: row.released_on,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            updated_by_username: row.updated_by_username,
            is_edited,
        }
    }
}

fn error(message: impl Into<String>) -> ValidationError {
    let mut error = ValidationError::new("invalid_release_note");
    error.message = Some(Cow::from(message.into()));
    error
}

/// `MAJOR.MINOR.PATCH` with an optional `-prerelease` of lower-case
/// letters, digits and dots.
pub fn validate_version(version: &str) -> Result<(), ValidationError> {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = version.len() <= 20
        && parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    let pre_ok = pre.is_none_or(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
    });
    if core_ok && pre_ok {
        Ok(())
    } else {
        Err(error("The version must look like 1.2.3 or 1.2.3-beta.1."))
    }
}

fn validate_title(value: &Value) -> Result<(), ValidationError> {
    validate_localized_map(value, &["en", "pt-BR"], 3, 120)
}

fn validate_items(items: &[ReleaseNoteItem]) -> Result<(), ValidationError> {
    if items.is_empty() || items.len() > 40 {
        return Err(error("A release needs between 1 and 40 items."));
    }
    for item in items {
        if !ITEM_KINDS.contains(&item.kind.as_str()) {
            return Err(error(format!("Unknown item kind '{}'.", item.kind)));
        }
        validate_localized_map(&item.text, &["en", "pt-BR"], 3, 600)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct CreateReleaseNotePayload {
    #[validate(custom(function = "validate_version"))]
    pub version: String,
    #[validate(custom(function = "validate_title"))]
    pub title: Value,
    #[validate(custom(function = "validate_items"))]
    pub items: Vec<ReleaseNoteItem>,
    pub released_on: NaiveDate,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, Validate)]
pub struct UpdateReleaseNotePayload {
    #[validate(custom(function = "validate_version"))]
    pub version: Option<String>,
    #[validate(custom(function = "validate_title"))]
    pub title: Option<Value>,
    #[validate(custom(function = "validate_items"))]
    pub items: Option<Vec<ReleaseNoteItem>>,
    pub released_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct PublishReleaseNotePayload {
    /// Notify every user in the app (and by e-mail, for those who opted
    /// in to product updates).
    #[serde(default)]
    pub notify: bool,
}

/// Trims the localized texts of every item.
pub fn trim_items(items: &[ReleaseNoteItem]) -> Vec<ReleaseNoteItem> {
    items
        .iter()
        .map(|item| ReleaseNoteItem {
            kind: item.kind.clone(),
            text: crate::models::billing::trim_localized(&item.text),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions() {
        for v in ["0.12.0", "1.2.3-beta.1", "10.0.0-rc1"] {
            assert!(validate_version(v).is_ok(), "{v}");
        }
        for v in ["1.2", "v1.2.3", "1.2.3-", "1.2.3-Beta", "1.2.x", ""] {
            assert!(validate_version(v).is_err(), "{v}");
        }
    }

    #[test]
    fn items_are_validated() {
        let item = |kind: &str| ReleaseNoteItem {
            kind: kind.into(),
            text: json!({ "en": "Tours arrived", "pt-BR": "Chegaram as turnês" }),
        };
        assert!(validate_items(&[item("new")]).is_ok());
        assert!(validate_items(&[item("bogus")]).is_err());
        assert!(validate_items(&[]).is_err());
        assert!(validate_items(&vec![item("new"); 41]).is_err());
        let short = ReleaseNoteItem {
            kind: "new".into(),
            text: json!({ "en": "ok", "pt-BR": "ok" }),
        };
        assert!(validate_items(&[short]).is_err());
    }

    #[test]
    fn edited_flag() {
        let now = chrono::Utc::now().naive_utc();
        let row = |updated: NaiveDateTime| ReleaseNoteRow {
            id: Uuid::new_v4(),
            version: "1.0.0".into(),
            title: json!({}),
            items: json!([]),
            released_on: now.date(),
            published_at: Some(now),
            created_at: now,
            updated_at: updated,
            updated_by_username: None,
        };
        assert!(!ReleaseNote::from(row(now + Duration::seconds(30))).is_edited);
        assert!(ReleaseNote::from(row(now + Duration::minutes(5))).is_edited);
    }
}
