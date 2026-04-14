use crate::{
    database::{
        AppState,
        repositories::song_repository::{SongRepository, SongRepositoryImpl},
    },
    errors::api_error::ApiError,
    models::DeletePayload,
};
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
    pub user_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(ToSchema, Clone, FromRow, Serialize, Deserialize)]
pub struct SongPublic {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub user_id: Uuid,
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
    pub fn new(title: &str, artist_id: Uuid, user_id: Uuid) -> Self {
        // REMOVIDO OPTION
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

    pub async fn count(state: &AppState, user_id: Uuid) -> Result<i64, ApiError> {
        SongRepositoryImpl::count(state, user_id).await
    }

    pub async fn find_all(state: &AppState, user_id: Uuid) -> Result<Vec<SongPublic>, ApiError> {
        SongRepositoryImpl::find_all(state, user_id).await
    }

    pub async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SongPublic>, ApiError> {
        SongRepositoryImpl::find_by_id(state, id, user_id).await
    }

    pub async fn create(
        state: &AppState,
        payload: &CreateSongPayload,
        user_id: Uuid,
    ) -> Result<Song, ApiError> {
        SongRepositoryImpl::create(state, payload, user_id).await
    }

    pub async fn update(state: &AppState, payload: &UpdateSongPayload) -> Result<Uuid, ApiError> {
        SongRepositoryImpl::update(state, payload).await
    }

    pub async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        SongRepositoryImpl::delete(state, payload).await
    }
}
