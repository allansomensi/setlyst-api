use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Type};
use std::borrow::Cow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidationError};

/// The languages the frontend actually ships (see `i18n/routing.ts`).
pub const SUPPORTED_LANGUAGES: [&str; 3] = ["en", "pt-BR", "es"];

/// Upper bound for the serialized `ui_settings` object.
pub const MAX_UI_SETTINGS_BYTES: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize, Type, Clone, ToSchema)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_theme", rename_all = "lowercase")]
pub enum UserTheme {
    Light,
    Dark,
    System,
}

#[derive(Debug, Serialize, Deserialize, FromRow, ToSchema)]
pub struct UserPreferences {
    pub id: Uuid,
    pub user_id: Uuid,
    pub language: String,
    pub theme: UserTheme,
    pub live_mode_font_size: i32,
    /// Client-owned UI settings (live mode and PDF defaults, list sizes,
    /// "what's new" read state...). Always a JSON object.
    pub ui_settings: Value,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

fn validate_language(language: &str) -> Result<(), ValidationError> {
    if SUPPORTED_LANGUAGES.contains(&language) {
        return Ok(());
    }
    let mut error = ValidationError::new("unsupported_language");
    error.message = Some(Cow::from("Unsupported language."));
    Err(error)
}

fn validate_ui_settings(value: &Value) -> Result<(), ValidationError> {
    if !value.is_object() {
        let mut error = ValidationError::new("invalid_ui_settings");
        error.message = Some(Cow::from("UI settings must be a JSON object."));
        return Err(error);
    }

    if value.to_string().len() > MAX_UI_SETTINGS_BYTES {
        let mut error = ValidationError::new("invalid_ui_settings");
        error.message = Some(Cow::from("UI settings are too large."));
        return Err(error);
    }

    Ok(())
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdatePreferencesPayload {
    #[validate(custom(function = "validate_language"))]
    pub language: Option<String>,
    pub theme: Option<UserTheme>,
    #[validate(range(min = 50, max = 300))]
    pub live_mode_font_size: Option<i32>,
    /// Shallow-merged into the stored object: top-level keys present here
    /// replace the stored ones, keys set to `null` are removed, and keys
    /// not mentioned are kept.
    #[validate(custom(function = "validate_ui_settings"))]
    pub ui_settings: Option<Value>,
}

/// Shallow-merges `patch` into `base` (both JSON objects). `null` values in
/// the patch delete the key.
pub fn merge_ui_settings(base: &Value, patch: &Value) -> Value {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            if value.is_null() {
                merged.remove(key);
            } else {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_replaces_adds_and_removes_keys() {
        let base = json!({ "live": { "compact": true }, "pdf": { "font": 1 }, "keep": 1 });
        let patch = json!({ "live": { "compact": false }, "pdf": null, "new": "x" });
        assert_eq!(
            merge_ui_settings(&base, &patch),
            json!({ "live": { "compact": false }, "keep": 1, "new": "x" })
        );
    }

    #[test]
    fn ui_settings_must_be_a_bounded_object() {
        assert!(validate_ui_settings(&json!({})).is_ok());
        assert!(validate_ui_settings(&json!([1, 2])).is_err());
        let big = json!({ "blob": "x".repeat(MAX_UI_SETTINGS_BYTES) });
        assert!(validate_ui_settings(&big).is_err());
    }

    #[test]
    fn only_supported_languages_are_accepted() {
        assert!(validate_language("pt-BR").is_ok());
        assert!(validate_language("fr").is_err());
    }
}
