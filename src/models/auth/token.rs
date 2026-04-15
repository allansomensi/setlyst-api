use crate::models::user::{Role, Status};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: Uuid,
    pub username: String,
    pub role: Role,
    pub status: Status,
    pub exp: usize,
    pub iat: usize,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct VerifyTokenPayload {
    pub token: String,
}
