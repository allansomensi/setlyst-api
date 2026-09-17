use crate::{errors::api_error::ApiError, models::notification::Notification};
use sqlx::PgPool;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait NotificationRepository: Send + Sync {
    /// Persists a new notification. Fire-and-forget from the caller's
    /// perspective: notifications are a side effect of some other action,
    /// never its main outcome.
    async fn create(&self, notification: &Notification) -> Result<(), ApiError>;

    /// Lists a user's notifications, newest first, along with the total
    /// count matching `unread_only` for pagination.
    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
        unread_only: bool,
    ) -> Result<(Vec<Notification>, i64), ApiError>;

    async fn count_unread(&self, user_id: Uuid) -> Result<i64, ApiError>;

    /// Marks a single notification as read. Scoped to `user_id` so a user
    /// can never mark — or even detect the existence of — another user's
    /// notification.
    async fn mark_read(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;

    async fn mark_all_read(&self, user_id: Uuid) -> Result<u64, ApiError>;

    async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
}

pub struct NotificationRepositoryImpl {
    pub db: PgPool,
}

impl NotificationRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl NotificationRepository for NotificationRepositoryImpl {
    async fn create(&self, notification: &Notification) -> Result<(), ApiError> {
        sqlx::query(
            r#"
            INSERT INTO notifications (id, user_id, type, data, read_at, created_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(notification.id)
        .bind(notification.user_id)
        .bind(notification.notification_type)
        .bind(&notification.data)
        .bind(notification.read_at)
        .bind(notification.created_at)
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
        unread_only: bool,
    ) -> Result<(Vec<Notification>, i64), ApiError> {
        let offset = (page - 1) * per_page;

        let notifications = sqlx::query_as::<_, Notification>(
            r#"
            SELECT id, user_id, type, data, read_at, created_at
            FROM notifications
            WHERE user_id = $1 AND ($2 = FALSE OR read_at IS NULL)
            ORDER BY created_at DESC
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(user_id)
        .bind(unread_only)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&self.db)
        .await?;

        let total_items = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*) FROM notifications
            WHERE user_id = $1 AND ($2 = FALSE OR read_at IS NULL)
            "#,
        )
        .bind(user_id)
        .bind(unread_only)
        .fetch_one(&self.db)
        .await?;

        Ok((notifications, total_items))
    }

    async fn count_unread(&self, user_id: Uuid) -> Result<i64, ApiError> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?;

        Ok(count)
    }

    async fn mark_read(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            r#"
            UPDATE notifications SET read_at = $1
            WHERE id = $2 AND user_id = $3 AND read_at IS NULL
            "#,
        )
        .bind(chrono::Utc::now().naive_utc())
        .bind(id)
        .bind(user_id)
        .execute(&self.db)
        .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }

    async fn mark_all_read(&self, user_id: Uuid) -> Result<u64, ApiError> {
        let result = sqlx::query(
            "UPDATE notifications SET read_at = $1 WHERE user_id = $2 AND read_at IS NULL",
        )
        .bind(chrono::Utc::now().naive_utc())
        .bind(user_id)
        .execute(&self.db)
        .await?;

        Ok(result.rows_affected())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM notifications WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }

        Ok(())
    }
}
