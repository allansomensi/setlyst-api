use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Song {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub user_id: Option<Uuid>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(ToSchema, Clone, FromRow, Serialize, Deserialize)]
pub struct SongPublic {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub user_id: Option<Uuid>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSongPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Title must be between 1 and 255 characters."
    ))]
    pub title: String,
    pub artist_id: Uuid,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSongPayload {
    pub id: Uuid,
    #[validate(length(
        min = 1,
        max = 255,
        message = "Title must be between 1 and 255 characters."
    ))]
    pub title: Option<String>,
    pub artist_id: Option<Uuid>,
}

impl Song {
    pub fn new(title: &str, artist_id: Uuid, user_id: Option<Uuid>) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: title.to_string(),
            artist_id,
            user_id,
            created_at: now,
            updated_at: now,
        }
    }
}
