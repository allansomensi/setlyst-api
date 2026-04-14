use crate::{
    database::{
        AppState,
        repositories::artist_repository::{ArtistRepository, ArtistRepositoryImpl},
    },
    errors::api_error::ApiError,
};
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

#[derive(ToSchema, Clone, FromRow, Serialize, Deserialize)]
pub struct ArtistPublic {
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
    pub name: String,
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

    pub async fn count(state: &AppState, user_id: Uuid) -> Result<i64, ApiError> {
        ArtistRepositoryImpl::count(state, user_id).await
    }

    pub async fn find_all(state: &AppState, user_id: Uuid) -> Result<Vec<ArtistPublic>, ApiError> {
        ArtistRepositoryImpl::find_all(state, user_id).await
    }

    pub async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<ArtistPublic>, ApiError> {
        ArtistRepositoryImpl::find_by_id(state, id, user_id).await
    }

    pub async fn create(
        state: &AppState,
        payload: &CreateArtistPayload,
        user_id: Uuid,
    ) -> Result<Artist, ApiError> {
        ArtistRepositoryImpl::create(state, payload, user_id).await
    }

    pub async fn update(
        state: &AppState,
        id: Uuid,
        payload: &UpdateArtistPayload,
    ) -> Result<Uuid, ApiError> {
        ArtistRepositoryImpl::update(state, id, payload).await
    }

    pub async fn delete(state: &AppState, id: Uuid) -> Result<(), ApiError> {
        ArtistRepositoryImpl::delete(state, id).await
    }
}
