use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        setlist::{CreateSetlistPayload, Setlist, SetlistPublic, UpdateSetlistPayload},
        song::SongPublic,
    },
};
use chrono::Utc;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SetlistRepository {
    async fn count(state: &AppState) -> Result<i64, ApiError>;
    async fn find_all(state: &AppState) -> Result<Vec<SetlistPublic>, ApiError>;
    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<SetlistPublic>, ApiError>;
    async fn create(
        state: &AppState,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Uuid, ApiError>;
    async fn update(state: &AppState, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError>;
}

pub struct SetlistRepositoryImpl;

#[async_trait::async_trait]
impl SetlistRepository for SetlistRepositoryImpl {
    async fn count(state: &AppState) -> Result<i64, ApiError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM setlists;")
            .fetch_one(&state.db)
            .await?;
        Ok(count)
    }

    async fn find_all(state: &AppState) -> Result<Vec<SetlistPublic>, ApiError> {
        let setlists = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists ORDER BY created_at DESC"
        )
        .fetch_all(&state.db)
        .await?;

        let mut result = Vec::new();
        for s in setlists {
            let songs = sqlx::query_as::<_, SongPublic>(
                "SELECT s.* FROM songs s JOIN setlist_songs ss ON s.id = ss.song_id WHERE ss.setlist_id = $1 ORDER BY ss.position",
            )
            .bind(s.id)
            .fetch_all(&state.db)
            .await?;

            result.push(SetlistPublic {
                id: s.id,
                title: s.title,
                description: s.description,
                user_id: s.user_id,
                songs,
                created_at: s.created_at,
                updated_at: s.updated_at,
            });
        }
        Ok(result)
    }

    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<SetlistPublic>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;

        if let Some(s) = setlist {
            let songs = sqlx::query_as::<_, SongPublic>(
                "SELECT s.* FROM songs s JOIN setlist_songs ss ON s.id = ss.song_id WHERE ss.setlist_id = $1 ORDER BY ss.position",
            )
            .bind(id)
            .fetch_all(&state.db)
            .await?;

            Ok(Some(SetlistPublic {
                id: s.id,
                title: s.title,
                description: s.description,
                user_id: s.user_id,
                songs,
                created_at: s.created_at,
                updated_at: s.updated_at,
            }))
        } else {
            Ok(None)
        }
    }

    async fn create(
        state: &AppState,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Uuid, ApiError> {
        let mut tx = state.db.begin().await?;
        let setlist_id = Uuid::new_v4();
        let now = Utc::now().naive_utc();

        sqlx::query("INSERT INTO setlists (id, title, description, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(setlist_id)
            .bind(&payload.title)
            .bind(&payload.description)
            .bind(user_id)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;

        for (pos, song_id) in payload.song_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)",
            )
            .bind(setlist_id)
            .bind(song_id)
            .bind(pos as i32)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(setlist_id)
    }

    async fn update(state: &AppState, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError> {
        let mut tx = state.db.begin().await?;
        let now = Utc::now().naive_utc();

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE setlists SET title = $1, updated_at = $2 WHERE id = $3")
                .bind(title)
                .bind(now)
                .bind(payload.id)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE setlists SET description = $1, updated_at = $2 WHERE id = $3")
                .bind(description)
                .bind(now)
                .bind(payload.id)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(song_ids) = &payload.song_ids {
            sqlx::query("DELETE FROM setlist_songs WHERE setlist_id = $1")
                .bind(payload.id)
                .execute(&mut *tx)
                .await?;

            for (pos, song_id) in song_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)",
                )
                .bind(payload.id)
                .bind(song_id)
                .bind(pos as i32)
                .execute(&mut *tx)
                .await?;
            }

            sqlx::query("UPDATE setlists SET updated_at = $1 WHERE id = $2")
                .bind(now)
                .bind(payload.id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(payload.id)
    }

    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM setlists WHERE id = $1")
            .bind(payload.id)
            .execute(&state.db)
            .await?;
        Ok(())
    }
}
