use super::DeletePayload;
use crate::{
    database::{
        AppState,
        repositories::user_repository::{UserRepository, UserRepositoryImpl},
    },
    errors::api_error::ApiError,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, PartialEq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Moderator,
    Admin,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Role::User => "user",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
        };
        f.write_str(s)
    }
}

#[derive(ToSchema, PartialEq, Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "user_status", rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Inactive,
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(ToSchema, Clone, FromRow, Serialize, Deserialize)]
pub struct UserPublic {
    pub id: Uuid,
    pub username: String,
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub role: Role,
    pub status: Status,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct RegisterPayload {
    #[validate(length(
        min = 3,
        max = 20,
        message = "Username must be between 3 and 20 chars."
    ))]
    pub username: String,
    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,
    #[validate(length(
        min = 8,
        max = 100,
        message = "Password must be between 8 and 100 chars."
    ))]
    pub password: String,
    #[validate(length(
        min = 3,
        max = 20,
        message = "First name must be between 3 and 20 chars."
    ))]
    pub first_name: Option<String>,
    #[validate(length(
        min = 3,
        max = 20,
        message = "Last name must be between 3 and 20 chars."
    ))]
    pub last_name: Option<String>,
}

impl From<RegisterPayload> for CreateUserPayload {
    fn from(value: RegisterPayload) -> Self {
        Self {
            username: value.username,
            email: value.email,
            password: value.password,
            first_name: value.first_name,
            last_name: value.last_name,
            role: Some(Role::default()),
            status: Some(Status::default()),
        }
    }
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateUserPayload {
    #[validate(length(
        min = 3,
        max = 20,
        message = "Username must be between 3 and 20 chars."
    ))]
    pub username: String,
    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,
    #[validate(length(
        min = 8,
        max = 100,
        message = "Password must be between 8 and 100 chars."
    ))]
    pub password: String,
    #[validate(length(
        min = 3,
        max = 20,
        message = "First name must be between 3 and 20 chars."
    ))]
    pub first_name: Option<String>,
    #[validate(length(
        min = 3,
        max = 20,
        message = "Last name must be between 3 and 20 chars."
    ))]
    pub last_name: Option<String>,
    pub role: Option<Role>,
    pub status: Option<Status>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateUserPayload {
    pub id: Uuid,
    #[validate(length(
        min = 3,
        max = 20,
        message = "Username must be between 3 and 20 chars."
    ))]
    pub username: Option<String>,
    #[validate(email(message = "Invalid email"))]
    pub email: Option<String>,
    #[validate(length(
        min = 8,
        max = 100,
        message = "Password must be between 8 and 100 chars."
    ))]
    pub password: Option<String>,
    #[validate(length(
        min = 3,
        max = 20,
        message = "First name must be between 3 and 20 chars."
    ))]
    pub first_name: Option<String>,
    #[validate(length(
        min = 3,
        max = 20,
        message = "Last name must be between 3 and 20 chars."
    ))]
    pub last_name: Option<String>,
    pub role: Option<Role>,
    pub status: Option<Status>,
}

impl User {
    pub fn new(
        username: &str,
        email: Option<String>,
        password: &str,
        first_name: Option<String>,
        last_name: Option<String>,
        role: Option<Role>,
        status: Option<Status>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            username: username.to_string(),
            email,
            password_hash: password.to_string(),
            first_name,
            last_name,
            role: role.unwrap_or_default(),
            status: status.unwrap_or_default(),
            created_at: Utc::now().naive_utc(),
            updated_at: Utc::now().naive_utc(),
        }
    }

    pub async fn count(state: &AppState) -> Result<i64, ApiError> {
        UserRepositoryImpl::count(state).await
    }

    pub async fn find_all(state: &AppState) -> Result<Vec<UserPublic>, ApiError> {
        UserRepositoryImpl::find_all(state).await
    }

    pub async fn find_by_id(state: &AppState, id: Uuid) -> Result<Option<UserPublic>, ApiError> {
        UserRepositoryImpl::find_by_id(state, id).await
    }

    pub async fn create(state: &AppState, payload: &CreateUserPayload) -> Result<Self, ApiError> {
        UserRepositoryImpl::create(state, payload).await
    }

    pub async fn update(state: &AppState, payload: &UpdateUserPayload) -> Result<Uuid, ApiError> {
        UserRepositoryImpl::update(state, payload).await
    }

    pub async fn delete(state: &AppState, payload: &DeletePayload) -> Result<(), ApiError> {
        UserRepositoryImpl::delete(state, payload).await
    }
}
