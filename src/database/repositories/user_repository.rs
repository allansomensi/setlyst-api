use crate::{
    errors::api_error::ApiError,
    models::user::{CreateUserPayload, UpdateUserPayload, User, UserPublic},
    utils::hashing::encrypt_password,
};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait UserRepository: Send + Sync {
    async fn find_all(&self, page: i64, size: i64) -> Result<(Vec<UserPublic>, i64), ApiError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserPublic>, ApiError>;
    async fn find_by_username(&self, username: &str) -> Result<Option<User>, ApiError>;
    async fn create(&self, payload: &CreateUserPayload) -> Result<User, ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateUserPayload) -> Result<Uuid, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn is_unique(&self, username: &str, exclude_id: Option<Uuid>) -> Result<(), ApiError>;
    async fn exists(&self, user_id: Uuid) -> Result<(), ApiError>;
}

pub struct UserRepositoryImpl {
    pub db: PgPool,
}

impl UserRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl UserRepository for UserRepositoryImpl {
    async fn find_all(&self, page: i64, size: i64) -> Result<(Vec<UserPublic>, i64), ApiError> {
        let offset = (page - 1) * size;
        let count = sqlx::query_scalar("SELECT COUNT(*) FROM users;").fetch_one(&self.db);
        let users = sqlx::query_as::<_, UserPublic>(
            r#"SELECT id, username, email, first_name, last_name, role, status, created_at, updated_at
            FROM users ORDER BY username ASC LIMIT $1 OFFSET $2"#,
        )
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, users) = tokio::try_join!(count, users)?;
        Ok((users, count))
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<UserPublic>, ApiError> {
        let user = sqlx::query_as::<_, UserPublic>(
            r#"SELECT id, username, email, first_name, last_name, role, status, created_at, updated_at
            FROM users WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(user)
    }

    async fn find_by_username(&self, username: &str) -> Result<Option<User>, ApiError> {
        let user = sqlx::query_as::<_, User>(
        "SELECT id, username, email, password_hash, first_name, last_name, role, status, created_at, updated_at FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(&self.db)
    .await?;

        Ok(user)
    }

    async fn create(&self, payload: &CreateUserPayload) -> Result<User, ApiError> {
        let new_user = User::new(
            &payload.username,
            payload.email.clone(),
            encrypt_password(&payload.password)?.as_str(),
            payload.first_name.clone(),
            payload.last_name.clone(),
            payload.role.clone(),
            payload.status.clone(),
        );

        sqlx::query(
            r#"INSERT INTO users (id, username, email, password_hash, first_name, last_name, role, status, created_at, updated_at) 
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(new_user.id)
        .bind(&new_user.username)
        .bind(&new_user.email)
        .bind(&new_user.password_hash)
        .bind(&new_user.first_name)
        .bind(&new_user.last_name)
        .bind(&new_user.role)
        .bind(&new_user.status)
        .bind(new_user.created_at)
        .bind(new_user.updated_at)
        .execute(&self.db)
        .await?;

        Ok(new_user)
    }

    async fn update(&self, id: Uuid, payload: &UpdateUserPayload) -> Result<Uuid, ApiError> {
        let mut updated = false;

        if let Some(username) = &payload.username {
            sqlx::query("UPDATE users SET username = $1 WHERE id = $2;")
                .bind(username)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(email) = &payload.email {
            sqlx::query("UPDATE users SET email = $1 WHERE id = $2;")
                .bind(email)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(password) = &payload.password {
            let encrypted_password = encrypt_password(password)?;
            sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2;")
                .bind(&encrypted_password)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(first_name) = &payload.first_name {
            sqlx::query("UPDATE users SET first_name = $1 WHERE id = $2;")
                .bind(first_name)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(last_name) = &payload.last_name {
            sqlx::query("UPDATE users SET last_name = $1 WHERE id = $2;")
                .bind(last_name)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(role) = &payload.role {
            sqlx::query("UPDATE users SET role = $1 WHERE id = $2;")
                .bind(role)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if let Some(status) = &payload.status {
            sqlx::query("UPDATE users SET status = $1 WHERE id = $2;")
                .bind(status)
                .bind(id)
                .execute(&self.db)
                .await?;
            updated = true;
        }

        if updated {
            sqlx::query("UPDATE users SET updated_at = $1 WHERE id = $2;")
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&self.db)
                .await?;
        } else {
            return Err(ApiError::NotModified);
        }

        Ok(id)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM users WHERE id = $1;")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn is_unique(&self, username: &str, exclude_id: Option<Uuid>) -> Result<(), ApiError> {
        let exists = match exclude_id {
            Some(id) => sqlx::query("SELECT id FROM users WHERE username = $1 AND id != $2;")
                .bind(username)
                .bind(id)
                .fetch_optional(&self.db)
                .await?
                .is_some(),
            None => sqlx::query("SELECT id FROM users WHERE username = $1;")
                .bind(username)
                .fetch_optional(&self.db)
                .await?
                .is_some(),
        };

        if exists {
            error!("Username '{username}' already exists.");
            Err(ApiError::AlreadyExists)
        } else {
            Ok(())
        }
    }

    async fn exists(&self, user_id: Uuid) -> Result<(), ApiError> {
        let exists = sqlx::query("SELECT id FROM users WHERE id = $1;")
            .bind(user_id)
            .fetch_optional(&self.db)
            .await?
            .is_some();

        if !exists {
            error!("User ID not found.");
            Err(ApiError::NotFound)
        } else {
            Ok(())
        }
    }
}
