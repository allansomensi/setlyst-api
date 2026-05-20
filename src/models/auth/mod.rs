use crate::validations::{password::validate_password, username::validate_username};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

pub mod access;
pub mod token;

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct LoginPayload {
    #[validate(custom(function = "validate_username"))]
    pub username: String,

    #[validate(custom(function = "validate_password"))]
    pub password: String,
}
