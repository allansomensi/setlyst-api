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
        let mut updated = false;

        if let Some(title) = &payload.title {
            sqlx::query("UPDATE setlists SET title = $1 WHERE id = $2")
                .bind(title)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(description) = &payload.description {
            sqlx::query("UPDATE setlists SET description = $1 WHERE id = $2")
                .bind(description)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE setlists SET updated_at = $1 WHERE id = $2")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&self.db)
                .await?;
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
}
