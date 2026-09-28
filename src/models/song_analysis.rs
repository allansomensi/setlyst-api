//! A song's manual harmonic analysis: chord degrees, resolutions, modal
//! borrowings and notes. The web app owns the document's shape; the API
//! stores it as an opaque, bounded JSON object, one per song.

use crate::{errors::api_error::ApiError, models::user_preferences::json_depth};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::prelude::FromRow;
use std::borrow::Cow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{ValidationError, ValidationErrors};

/// Upper bound for the serialized analysis document.
pub const MAX_SONG_ANALYSIS_BYTES: usize = 256 * 1024;
/// Deepest nesting accepted in the document (the top-level object is 1).
pub const MAX_SONG_ANALYSIS_DEPTH: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, ToSchema)]
pub struct SongAnalysis {
    pub song_id: Uuid,
    /// The analysis document, as the client saved it. Always a JSON object.
    #[schema(value_type = Object)]
    pub content: Value,
    pub created_at: NaiveDateTime,
    /// Also the version to send back as `base_updated_at` when saving.
    pub updated_at: NaiveDateTime,
    /// `null` once that account is deleted.
    pub updated_by_username: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct SaveSongAnalysisPayload {
    /// The whole document (replaces the stored one): a JSON object of at
    /// most 256 KiB, nested at most 8 levels deep.
    #[schema(value_type = Object)]
    pub content: Value,
    /// The `updated_at` of the analysis the client edited. When set and
    /// the stored analysis has changed since, the save is refused
    /// (`ANALYSIS_CONFLICT`, 409). Leave it out to overwrite whatever is
    /// there.
    #[serde(default)]
    pub base_updated_at: Option<NaiveDateTime>,
}

/// Why `content` can't be stored, if it can't.
pub fn check_content(content: &Value) -> Result<(), ValidationError> {
    let fail = |message: &'static str| {
        let mut error = ValidationError::new("invalid_analysis");
        error.message = Some(Cow::from(message));
        Err(error)
    };
    if !content.is_object() {
        return fail("The analysis must be a JSON object.");
    }
    if content.to_string().len() > MAX_SONG_ANALYSIS_BYTES {
        return fail("The analysis is too large.");
    }
    if json_depth(content) > MAX_SONG_ANALYSIS_DEPTH {
        return fail("The analysis is nested too deeply.");
    }
    Ok(())
}

impl SaveSongAnalysisPayload {
    /// A regular `VALIDATION_ERROR` on the `content` field.
    pub fn validate(&self) -> Result<(), ApiError> {
        check_content(&self.content).map_err(|error| {
            let mut errors = ValidationErrors::new();
            errors.add("content", error);
            ApiError::ValidationError(errors)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_must_be_an_object() {
        assert!(check_content(&json!({})).is_ok());
        assert!(check_content(&json!({ "chords": [{ "degree": "V7" }] })).is_ok());
        for value in [json!([]), json!("x"), json!(1), json!(null), json!(true)] {
            assert!(check_content(&value).is_err(), "{value}");
        }
    }

    #[test]
    fn content_size_is_bounded() {
        let fits = "x".repeat(MAX_SONG_ANALYSIS_BYTES - 20);
        assert!(check_content(&json!({ "n": fits })).is_ok());
        let too_big = "x".repeat(MAX_SONG_ANALYSIS_BYTES);
        assert!(check_content(&json!({ "n": too_big })).is_err());
    }

    #[test]
    fn content_depth_is_bounded() {
        // 8 levels: the object and 7 nested arrays.
        let deep_ok = json!({ "a": [[[[[[[1]]]]]]] });
        assert_eq!(json_depth(&deep_ok), 8);
        assert!(check_content(&deep_ok).is_ok());
        let too_deep = json!({ "a": [[[[[[[[1]]]]]]]] });
        assert!(check_content(&too_deep).is_err());
    }

    #[test]
    fn payload_errors_are_validation_errors() {
        let payload = SaveSongAnalysisPayload {
            content: json!([1]),
            base_updated_at: None,
        };
        let error = payload.validate().unwrap_err();
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }
}
