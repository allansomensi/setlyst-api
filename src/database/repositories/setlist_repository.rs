use crate::{
    errors::api_error::ApiError,
    models::setlist::{CreateSetlistPayload, Setlist, UpdateSetlistPayload},
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SetlistRepository: Send + Sync {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError>;
    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError>;
    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        position: i32,
    ) -> Result<(), ApiError>;
    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError>;
    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<crate::models::song::Song>, i64), ApiError>;
    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError>;
}

pub struct SetlistRepositoryImpl {
    pub db: PgPool,
}

impl SetlistRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl SetlistRepository for SetlistRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Setlist>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM setlists WHERE user_id = $1;")
            .bind(user_id)
            .fetch_one(&self.db);

        let setlists = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists WHERE user_id = $1 ORDER BY title ASC LIMIT $2 OFFSET $3",
        )
        .bind(user_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, setlists) = tokio::try_join!(count, setlists)?;
        Ok((setlists, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Setlist>, ApiError> {
        let setlist = sqlx::query_as::<_, Setlist>(
            "SELECT id, title, description, user_id, created_at, updated_at FROM setlists WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(setlist)
    }

    async fn create(
        &self,
        payload: &CreateSetlistPayload,
        user_id: Uuid,
    ) -> Result<Setlist, ApiError> {
        let new_setlist = Setlist::new(&payload.title, payload.description.clone(), user_id);
        sqlx::query(
            "INSERT INTO setlists (id, title, description, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(new_setlist.id)
        .bind(&new_setlist.title)
        .bind(&new_setlist.description)
        .bind(new_setlist.user_id)
        .bind(new_setlist.created_at)
        .bind(new_setlist.updated_at)
        .execute(&self.db)
        .await?;
        Ok(new_setlist)
    }

    async fn update(&self, id: Uuid, payload: &UpdateSetlistPayload) -> Result<Uuid, ApiError> {
        let mut tx = self.db.begin().await?;
        let mut updated = false;

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE setlists SET title = $1 WHERE id = $2")
                .bind(title)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE setlists SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE setlists SET updated_at = $1 WHERE id = $2")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(id)
        } else {
            Err(ApiError::NotModified)
        }
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM setlists WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(
        &self,
        title: &str,
        user_id: Uuid,
        exclude_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let exists = match exclude_id {
            Some(id) => sqlx::query(
                "SELECT id FROM setlists WHERE title = $1 AND user_id = $2 AND id != $3;",
            )
            .bind(title)
            .bind(user_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await?
            .is_some(),
            None => sqlx::query("SELECT id FROM setlists WHERE title = $1 AND user_id = $2;")
                .bind(title)
                .bind(user_id)
                .fetch_optional(&self.db)
                .await?
                .is_some(),
        };

        if exists {
            error!("Setlist '{title}' already exists for this user.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM setlists WHERE id = $1 AND user_id = $2;")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some();

        if !exists {
            error!("Setlist ID not found or unauthorized.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn add_song(
        &self,
        setlist_id: Uuid,
        song_id: Uuid,
        position: i32,
    ) -> Result<(), ApiError> {
        sqlx::query(
            "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1, $2, $3)
             ON CONFLICT (setlist_id, song_id) DO UPDATE SET position = $3;",
        )
        .bind(setlist_id)
        .bind(song_id)
        .bind(position)
        .execute(&self.db)
        .await
        .map_err(|e| {
            tracing::error!("Error adding song to setlist: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(())
    }

    async fn remove_song(&self, setlist_id: Uuid, song_id: Uuid) -> Result<(), ApiError> {
        let result =
            sqlx::query("DELETE FROM setlist_songs WHERE setlist_id = $1 AND song_id = $2;")
                .bind(setlist_id)
                .bind(song_id)
                .execute(&self.db)
                .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn get_songs(
        &self,
        setlist_id: Uuid,
        page: i64,
        size: i64,
    ) -> Result<(Vec<crate::models::song::Song>, i64), ApiError> {
        let offset = (page - 1) * size;

        let count = sqlx::query_scalar("SELECT COUNT(*) FROM setlist_songs WHERE setlist_id = $1;")
            .bind(setlist_id)
            .fetch_one(&self.db);

        let songs = sqlx::query_as::<_, crate::models::song::Song>(
            "SELECT s.* FROM songs s
             INNER JOIN setlist_songs ss ON s.id = ss.song_id
             WHERE ss.setlist_id = $1
             ORDER BY ss.position ASC
             LIMIT $2 OFFSET $3;",
        )
        .bind(setlist_id)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, songs) = tokio::try_join!(count, songs)?;
        Ok((songs, count))
    }

    async fn reorder_songs(&self, setlist_id: Uuid, song_ids: &[Uuid]) -> Result<(), ApiError> {
        sqlx::query(
            r#"
            UPDATE setlist_songs AS ss
            SET position = u.new_position
            FROM (
                SELECT unnest($1::uuid[]) AS id, 
                       generate_series(1, array_length($1::uuid[], 1)) AS new_position
            ) AS u
            WHERE ss.setlist_id = $2 AND ss.song_id = u.id
            "#,
        )
        .bind(song_ids)
        .bind(setlist_id)
        .execute(&self.db)
        .await
        .map_err(|e| {
            tracing::error!("Failed to reorder setlist songs: {e}");
            ApiError::DatabaseError(e)
        })?;

        Ok(())
    }
}
