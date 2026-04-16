use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Setlist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub user_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    pub description: Option<String>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct AddSongToSetlistPayload {
    pub song_id: Uuid,
    pub position: i32,
}

impl Setlist {
    pub fn new(title: &str, description: Option<String>, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: title.to_string(),
            description,
            user_id,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReorderSetlistSongsPayload {
    #[validate(length(min = 1, message = "The list of song IDs cannot be empty."))]
    pub song_ids: Vec<Uuid>,
}
