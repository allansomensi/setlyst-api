use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub iat: usize,
    pub sub: String,
    pub exp: usize,
    pub role: String,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct VerifyTokenPayload {
    pub token: String,
}
