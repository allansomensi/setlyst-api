use crate::{errors::api_error::ApiError, models::user_note::UserStaffNote};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

macro_rules! note_columns {
    () => {
        "id, user_id, author_id, author_username, body, pinned, created_at, updated_at"
    };
}

#[async_trait::async_trait]
pub trait UserNoteRepository: Send + Sync {
    /// Notes on `user_id`, pinned first, newest first.
    async fn list(&self, user_id: Uuid) -> Result<Vec<UserStaffNote>, ApiError>;
    async fn find(&self, user_id: Uuid, id: Uuid) -> Result<Option<UserStaffNote>, ApiError>;
    async fn create(
        &self,
        user_id: Uuid,
        author_id: Uuid,
        author_username: &str,
        body: &str,
        pinned: bool,
    ) -> Result<UserStaffNote, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        body: Option<&str>,
        pinned: Option<bool>,
    ) -> Result<UserStaffNote, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Notes on `user_id` (shown as a count in the user list).
    async fn count(&self, user_id: Uuid) -> Result<i64, ApiError>;
}

pub struct UserNoteRepositoryImpl {
    pub db: PgPool,
}

impl UserNoteRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl UserNoteRepository for UserNoteRepositoryImpl {
    async fn list(&self, user_id: Uuid) -> Result<Vec<UserStaffNote>, ApiError> {
        Ok(sqlx::query_as::<_, UserStaffNote>(concat!(
            "SELECT ",
            note_columns!(),
            " FROM user_staff_notes WHERE user_id = $1
             ORDER BY pinned DESC, created_at DESC"
        ))
        .bind(user_id)
        .fetch_all(&self.db)
        .await?)
    }

    async fn find(&self, user_id: Uuid, id: Uuid) -> Result<Option<UserStaffNote>, ApiError> {
        Ok(sqlx::query_as::<_, UserStaffNote>(concat!(
            "SELECT ",
            note_columns!(),
            " FROM user_staff_notes WHERE id = $1 AND user_id = $2"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?)
    }

    async fn create(
        &self,
        user_id: Uuid,
        author_id: Uuid,
        author_username: &str,
        body: &str,
        pinned: bool,
    ) -> Result<UserStaffNote, ApiError> {
        let now = Utc::now().naive_utc();
        Ok(sqlx::query_as::<_, UserStaffNote>(concat!(
            "INSERT INTO user_staff_notes (id, user_id, author_id, author_username, body, pinned,
                                           created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
             RETURNING ",
            note_columns!(),
            ""
        ))
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(author_id)
        .bind(author_username)
        .bind(body.trim())
        .bind(pinned)
        .bind(now)
        .fetch_one(&self.db)
        .await?)
    }

    async fn update(
        &self,
        id: Uuid,
        body: Option<&str>,
        pinned: Option<bool>,
    ) -> Result<UserStaffNote, ApiError> {
        Ok(sqlx::query_as::<_, UserStaffNote>(concat!(
            "UPDATE user_staff_notes
             SET body = COALESCE($2, body), pinned = COALESCE($3, pinned),
                 updated_at = CASE WHEN $2 IS NULL THEN updated_at ELSE $4 END
             WHERE id = $1
             RETURNING ",
            note_columns!(),
            ""
        ))
        .bind(id)
        .bind(body.map(str::trim))
        .bind(pinned)
        .bind(Utc::now().naive_utc())
        .fetch_one(&self.db)
        .await?)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM user_staff_notes WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn count(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM user_staff_notes WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&self.db)
                .await?,
        )
    }
}
