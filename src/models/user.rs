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

impl From<User> for UserPublic {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            email: user.email,
            first_name: user.first_name,
            last_name: user.last_name,
            role: user.role,
            status: user.status,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
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
    #[serde(skip_serializing)]
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
    #[serde(skip_serializing)]
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
    #[serde(skip_serializing)]
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
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            username: username.to_string(),
            email,
            password_hash: password.to_string(),
            first_name,
            last_name,
            role: role.unwrap_or_default(),
            status: status.unwrap_or_default(),
            created_at: now,
            updated_at: now,
        }
    }
}
