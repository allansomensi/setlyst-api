use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        artist::{Artist, ArtistPublic, CreateArtistPayload, UpdateArtistPayload},
    },
};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait ArtistRepository {
    async fn count(state: &AppState, user_id: Uuid) -> Result<i64, ApiError>;
    async fn find_all(state: &AppState, user_id: Uuid) -> Result<Vec<ArtistPublic>, ApiError>;
    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<ArtistPublic>, ApiError>;
    async fn create(
        state: &AppState,
        payload: &CreateArtistPayload,
        user_id: Uuid,
    ) -> Result<Artist, ApiError>;
    async fn update(state: &AppState, payload: &UpdateArtistPayload) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError>;
}

pub struct ArtistRepositoryImpl;

#[async_trait::async_trait]
impl ArtistRepository for ArtistRepositoryImpl {
    async fn count(state: &AppState, user_id: Uuid) -> Result<i64, ApiError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?;
        Ok(count)
    }

    async fn find_all(state: &AppState, user_id: Uuid) -> Result<Vec<ArtistPublic>, ApiError> {
        let artists = sqlx::query_as::<_, ArtistPublic>(
            "SELECT id, name, user_id, created_at, updated_at FROM artists WHERE user_id = $1 ORDER BY name ASC",
        )
        .bind(user_id)
        .fetch_all(&state.db)
        .await?;
        Ok(artists)
    }

    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<ArtistPublic>, ApiError> {
        let artist = sqlx::query_as::<_, ArtistPublic>(
            "SELECT id, name, user_id, created_at, updated_at FROM artists WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
        Ok(artist)
    }

    async fn create(
        state: &AppState,
        payload: &CreateArtistPayload,
        user_id: Uuid,
    ) -> Result<Artist, ApiError> {
        let new_artist = Artist::new(&payload.name, user_id);
        sqlx::query(
            "INSERT INTO artists (id, name, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(new_artist.id)
        .bind(&new_artist.name)
        .bind(new_artist.user_id)
        .bind(new_artist.created_at)
        .bind(new_artist.updated_at)
        .execute(&state.db)
        .await?;
        Ok(new_artist)
    }

    async fn update(state: &AppState, payload: &UpdateArtistPayload) -> Result<Uuid, ApiError> {
        sqlx::query("UPDATE artists SET name = $1, updated_at = $2 WHERE id = $3")
            .bind(&payload.name)
            .bind(chrono::Utc::now().naive_utc())
            .bind(payload.id)
            .execute(&state.db)
            .await?;
        Ok(payload.id)
    }

    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM artists WHERE id = $1")
            .bind(payload.id)
            .execute(&state.db)
            .await?;
        Ok(())
    }
}
