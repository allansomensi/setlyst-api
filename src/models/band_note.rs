//! Band reminders: short notes shown on the band page (rehearsal times,
//! deadlines, "bring the spare cables").

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Most reminders a band may keep.
pub const MAX_BAND_NOTES: i64 = 100;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "band_note_color", rename_all = "lowercase")]
pub enum BandNoteColor {
    #[default]
    Default,
    Yellow,
    Green,
    Blue,
    Red,
    Purple,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BandNoteAuthor {
    pub id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BandNote {
    pub id: Uuid,
    pub band_id: Uuid,
    /// `null` once the author's account is deleted.
    pub author: Option<BandNoteAuthor>,
    pub content: String,
    pub color: BandNoteColor,
    pub is_pinned: bool,
    pub due_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub updated_by_username: Option<String>,
    /// Whether the caller may edit or delete it (its author, or a band
    /// moderator or above).
    pub can_edit: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct BandNoteRow {
    pub id: Uuid,
    pub band_id: Uuid,
    pub author_id: Option<Uuid>,
    pub author_username: Option<String>,
    pub author_avatar_url: Option<String>,
    pub content: String,
    pub color: BandNoteColor,
    pub is_pinned: bool,
    pub due_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub updated_by_username: Option<String>,
}

impl BandNoteRow {
    pub fn into_note(self, can_edit: bool) -> BandNote {
        let author = match (self.author_id, self.author_username) {
            (Some(id), Some(username)) => Some(BandNoteAuthor {
                id,
                username,
                avatar_url: self.author_avatar_url,
            }),
            _ => None,
        };
        BandNote {
            id: self.id,
            band_id: self.band_id,
            author,
            content: self.content,
            color: self.color,
            is_pinned: self.is_pinned,
            due_at: self.due_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            updated_by_username: self.updated_by_username,
            can_edit,
        }
    }
}

fn validate_content(content: &str) -> Result<(), validator::ValidationError> {
    let len = content.trim().chars().count();
    if len == 0 || len > 2_000 {
        let mut error = validator::ValidationError::new("invalid_note_content");
        error.message = Some(std::borrow::Cow::from(
            "A reminder must have between 1 and 2000 characters.",
        ));
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateBandNotePayload {
    #[validate(custom(function = "validate_content"))]
    pub content: String,
    pub color: Option<BandNoteColor>,
    /// Pinning requires the `moderator` band role or above.
    pub is_pinned: Option<bool>,
    pub due_at: Option<NaiveDateTime>,
}

/// Absent = unchanged; `due_at: null` clears it.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateBandNotePayload {
    #[validate(custom(function = "validate_content"))]
    pub content: Option<String>,
    pub color: Option<BandNoteColor>,
    pub is_pinned: Option<bool>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    pub due_at: Option<Option<NaiveDateTime>>,
}
