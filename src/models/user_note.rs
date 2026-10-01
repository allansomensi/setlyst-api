//! Internal notes staff leave on an account.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::{Validate, ValidationError};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct UserStaffNote {
    pub id: Uuid,
    pub user_id: Uuid,
    pub author_id: Option<Uuid>,
    /// The author's username when the note was written.
    pub author_username: Option<String>,
    pub body: String,
    pub pinned: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

fn validate_note(body: &str) -> Result<(), ValidationError> {
    let length = body.trim().chars().count();
    if length == 0 || length > 2_000 {
        let mut error = ValidationError::new("length");
        error.message = Some("Must be between 1 and 2 000 characters.".into());
        return Err(error);
    }
    // Postgres text can't hold NUL: refused here instead of failing the
    // insert.
    if body.contains('\0') {
        let mut error = ValidationError::new("characters");
        error.message = Some("Contains characters that aren't allowed.".into());
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct CreateUserNotePayload {
    #[validate(custom(function = "validate_note"))]
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema, Validate)]
pub struct UpdateUserNotePayload {
    #[validate(custom(function = "validate_note"))]
    pub body: Option<String>,
    pub pinned: Option<bool>,
}
