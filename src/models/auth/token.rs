use crate::models::user::{Role, Status};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: Uuid,
    pub username: String,
    /// The role at issue time. Informational only for clients: the
    /// authentication middleware replaces it with the *current* role from
    /// the database on every request, so promotions and demotions apply
    /// immediately.
    pub role: Role,
    pub status: Status,
    pub exp: usize,
    pub iat: usize,
    /// The account's `token_version` when this token was issued. Tokens
    /// minted before the column existed deserialize as `0`, which matches
    /// the column default.
    #[serde(default)]
    pub ver: i32,
    /// Set on impersonation tokens: the staff member viewing the platform
    /// as `sub`. Such tokens are read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imp: Option<Uuid>,
    /// On impersonation tokens: the impersonator's `token_version` at
    /// issue time. Signing the staff member out everywhere (or changing
    /// their password) therefore also ends every "view as" session they
    /// opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iver: Option<i32>,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct VerifyTokenPayload {
    pub token: String,
}
