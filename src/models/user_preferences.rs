use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

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
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdatePreferencesPayload {
    #[validate(length(min = 2, max = 10))]
    pub language: Option<String>,
    pub theme: Option<UserTheme>,
    #[validate(range(min = 50, max = 300))]
    pub live_mode_font_size: Option<i32>,
}
