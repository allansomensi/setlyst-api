use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Artist {
    pub id: Uuid,
    pub name: String,
    pub user_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateArtistPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Artist name must be between 1 and 255 chars."
    ))]
    pub name: String,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateArtistPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Artist name must be between 1 and 255 chars."
    ))]
    pub name: Option<String>,
}

impl Artist {
    pub fn new(name: &str, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            user_id,
            created_at: now,
            updated_at: now,
        }
    }
}
