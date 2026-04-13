use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        DeletePayload,
        user::{CreateUserPayload, UpdateUserPayload, User, UserPublic},
    },
    utils::hashing::encrypt_password,
};
use tracing::{debug, info};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait UserRepository {
    async fn count(state: &AppState) -> Result<i64, ApiError>;
    async fn find_all(state: &AppState) -> Result<Vec<UserPublic>, ApiError>;
    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<UserPublic>, ApiError>;
    async fn create(state: &AppState, payload: &CreateUserPayload) -> Result<User, ApiError>;
    async fn update(state: &AppState, payload: &UpdateUserPayload) -> Result<Uuid, ApiError>;
    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError>;
}

pub struct UserRepositoryImpl;

#[async_trait::async_trait]
impl UserRepository for UserRepositoryImpl {
    async fn count(state: &AppState) -> Result<i64, ApiError> {
        debug!("Attempting to count users from the database...");

        let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM users;"#)
            .fetch_one(&state.db)
            .await?;

        Ok(count)
    }

    async fn find_all(state: &AppState) -> Result<Vec<UserPublic>, ApiError> {
        debug!("Attempting to retrieve all users from the database...");

        let users: Vec<UserPublic> = sqlx::query_as(
            r#"
        SELECT 
            id, username, email, first_name, last_name, role, status, created_at, updated_at
        FROM users;
        "#,
        )
        .fetch_all(&state.db)
        .await?;

        Ok(users)
    }

    async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<UserPublic>, ApiError> {
        debug!("Attempting to retrieve user with id: {id}");

        let user: Option<UserPublic> = sqlx::query_as(
            r#"
        SELECT 
            id, username, email, first_name, last_name, role, status, created_at, updated_at
        FROM users
        WHERE id = $1;
        "#,
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await?;

        Ok(user)
    }

    async fn create(state: &AppState, payload: &CreateUserPayload) -> Result<User, ApiError> {
        debug!(
            "Attempting to create user with username: {}",
            payload.username
        );

        let new_user = User::new(
            &payload.username,
            payload.email.clone(),
            encrypt_password(&payload.password)?.as_str(),
            payload.first_name.clone(),
            payload.last_name.clone(),
            payload.role.clone(),
            payload.status.clone(),
        );

        sqlx::query(r#"INSERT INTO users (id, username, email, password_hash, first_name, last_name, role, status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#)
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
    .execute(&state.db)
    .await?;

        Ok(new_user)
    }

    async fn update(state: &AppState, payload: &UpdateUserPayload) -> Result<Uuid, ApiError> {
        debug!("Attempting to update user with ID: {}", payload.id);

        let user_id = payload.id;
        let new_username = &payload.username;
        let new_email = &payload.email;
        let new_password = &payload.password;
        let new_role = &payload.role;
        let new_status = &payload.status;
        let new_first_name = &payload.first_name;
        let new_last_name = &payload.last_name;

        let mut updated = false;

        // Update `username` if provided.
        if let Some(username) = new_username {
            sqlx::query(r#"UPDATE users SET username = $1 WHERE id = $2;"#)
                .bind(username)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated username of user with ID: {}", payload.id);
            updated = true;
        }

        // Update `email` if provided.
        if let Some(email) = new_email {
            sqlx::query(r#"UPDATE users SET email = $1 WHERE id = $2;"#)
                .bind(email)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated email of user with ID: {}", payload.id);
            updated = true;
        }

        // Encrypt and update the `password` if provided
        if let Some(password) = new_password {
            let encrypted_password = encrypt_password(&password)?;

            sqlx::query(r#"UPDATE users SET password_hash = $1 WHERE id = $2;"#)
                .bind(&encrypted_password)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated password of user with ID: {}", payload.id);
            updated = true;
        }

        // Update `first_name` if provided
        if let Some(first_name) = new_first_name {
            sqlx::query(r#"UPDATE users SET first_name = $1 WHERE id = $2;"#)
                .bind(first_name)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated first_name of user with ID: {}", payload.id);
            updated = true;
        }

        // Update `last_name` if provided
        if let Some(last_name) = new_last_name {
            sqlx::query(r#"UPDATE users SET last_name = $1 WHERE id = $2;"#)
                .bind(last_name)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated last_name of user with ID: {}", payload.id);
            updated = true;
        }

        // Update `role` if provided
        if let Some(role) = new_role {
            sqlx::query(r#"UPDATE users SET role = $1 WHERE id = $2;"#)
                .bind(role)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated role of user with ID: {}", payload.id);
            updated = true;
        }

        // Update `status` if provided
        if let Some(status) = new_status {
            sqlx::query(r#"UPDATE users SET status = $1 WHERE id = $2;"#)
                .bind(status)
                .bind(user_id)
                .execute(&state.db)
                .await?;

            info!("Updated status of user with ID: {}", payload.id);
            updated = true;
        }

        // Updates `updated_at` field.
        if updated {
            sqlx::query(r#"UPDATE users SET updated_at = $1 WHERE id = $2;"#)
                .bind(chrono::Utc::now().naive_utc())
                .bind(user_id)
                .execute(&state.db)
                .await?;
        } else {
            return Err(ApiError::NotModified);
        }

        Ok(user_id)
    }

    async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        debug!("Attempting to delete user with ID: {}", payload.id);

        sqlx::query(r#"DELETE FROM users WHERE id = $1;"#)
            .bind(payload.id)
            .execute(&state.db)
            .await?;

        Ok(())
    }
}
