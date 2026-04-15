use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        setlist::{CreateSetlistPayload, Setlist, SetlistPublic, UpdateSetlistPayload},
        song::SongPublic,
    },
};
use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SetlistRepository {
    async fn find_all(
        state: &AppState,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SetlistPublic>, i64), ApiError>;
    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SetlistPublic>, ApiError>;
    async fn create(
        state: &AppState,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError>;
    async fn update(
        state: &AppState,
        id: Uuid,
        payload: &UpdateSetlistPayload,
    ) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, id: Uuid) -> Result<(), ApiError>;
}

pub struct SetlistRepositoryImpl;

#[async_trait::async_trait]
impl SetlistRepository for SetlistRepositoryImpl {
    async fn find_all(
        state: &AppState,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<SetlistPublic>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count_future = sqlx::query_scalar("SELECT COUNT(*) FROM setlists WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&state.db);

        let setlists_future = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&state.db);

        let (total_items, setlists) = tokio::try_join!(count_future, setlists_future)?;

        if setlists.is_empty() {
            return Ok((Vec::new(), total_items));
        }

        let setlist_ids: Vec<Uuid> = setlists.iter().map(|s| s.id).collect();

        #[derive(sqlx::FromRow)]
        struct SongWithSetlist {
            setlist_id: Uuid,
            #[sqlx(flatten)]
            song: SongPublic,
        }

        let songs_with_setlists = sqlx::query_as::<_, SongWithSetlist>(
            "SELECT ss.setlist_id, s.id, s.title, s.artist_id, s.user_id, s.created_at, s.updated_at 
             FROM songs s 
             JOIN setlist_songs ss ON s.id = ss.song_id 
             WHERE ss.setlist_id = ANY($1) 
             ORDER BY ss.position"
        )
        .bind(&setlist_ids)
        .fetch_all(&state.db)
        .await?;

        let mut songs_map: HashMap<Uuid, Vec<SongPublic>> = HashMap::new();
        for item in songs_with_setlists {
            songs_map
                .entry(item.setlist_id)
                .or_default()
                .push(item.song);
        }

        let result = setlists
            .into_iter()
            .map(|s| {
                let songs = songs_map.remove(&s.id).unwrap_or_default();
                SetlistPublic {
                    id: s.id,
                    title: s.title,
                    description: s.description,
                    user_id: s.user_id,
                    songs,
                    created_at: s.created_at,
                    updated_at: s.updated_at,
                }
            })
            .collect();

        Ok((result, total_items))
    }

    async fn find_by_id(
        state: &AppState,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SetlistPublic>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists WHERE id = $1 AND user_id = $2"
        )
        .bind(id)
        .bind(user_id)
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
    ) -> Result<Setlist, ApiError> {
        let mut tx = state.db.begin().await?;

        let new_setlist = Setlist::new(&payload.title, payload.description.clone(), user_id);

        sqlx::query("INSERT INTO setlists (id, title, description, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&mut *tx)
        .await?;

        for (pos, song_id) in payload.song_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)",
            )
            .bind(new_setlist.id)
            .bind(song_id)
            .bind(pos as i32)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        Ok(new_setlist)
    }

    async fn update(
        state: &AppState,
        id: Uuid,
        payload: &UpdateSetlistPayload,
    ) -> Result<Uuid, ApiError> {
        let mut tx = state.db.begin().await?;
        let now = Utc::now().naive_utc();

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE setlists SET title = $1, updated_at = $2 WHERE id = $3")
                .bind(title)
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE setlists SET description = $1, updated_at = $2 WHERE id = $3")
                .bind(description)
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        if let Some(song_ids) = &payload.song_ids {
            sqlx::query("DELETE FROM setlist_songs WHERE setlist_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;

            for (pos, song_id) in song_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)",
                )
                .bind(id)
                .bind(song_id)
                .bind(pos as i32)
                .execute(&mut *tx)
                .await?;
            }

            sqlx::query("UPDATE setlists SET updated_at = $1 WHERE id = $2")
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(id)
    }

    async fn delete(state: &AppState, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM setlists WHERE id = $1")
            .bind(id)
            .execute(&state.db)
            .await?;
        Ok(())
    }
}
