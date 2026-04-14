use crate::{
    database::{
        AppState,
        repositories::setlist_repository::{SetlistRepository, SetlistRepositoryImpl},
    },
    errors::api_error::ApiError,
    models::{DeletePayload, song::SongPublic},
};
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

#[derive(ToSchema, Clone, Serialize, Deserialize)]
pub struct SetlistPublic {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub user_id: Uuid,
    pub songs: Vec<SongPublic>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    pub description: Option<String>,
    pub song_ids: Vec<Uuid>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistPayload {
    pub id: Uuid,
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    pub description: Option<String>,
    pub song_ids: Option<Vec<Uuid>>,
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

    pub async fn count(state: &AppState) -> Result<i64, ApiError> {
        SetlistRepositoryImpl::count(state).await
    }

    pub async fn find_all(state: &AppState) -> Result<Vec<SetlistPublic>, ApiError> {
        SetlistRepositoryImpl::find_all(state).await
    }

    pub async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<SetlistPublic>, ApiError> {
        SetlistRepositoryImpl::find_by_id(state, id).await
    }

    pub async fn create(
        state: &AppState,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError> {
        SetlistRepositoryImpl::create(state, payload, user_id).await
    }

    pub async fn update(
        state: &AppState,
        payload: &UpdateSetlistPayload,
    ) -> Result<Uuid, ApiError> {
        SetlistRepositoryImpl::update(state, payload).await
    }

    pub async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        SetlistRepositoryImpl::delete(state, payload).await
    }
}
