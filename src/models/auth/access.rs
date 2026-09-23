use crate::{
    errors::api_error::ApiError,
    models::{auth::token::Claims, user::Role},
};
use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

/// Wrapper struct providing authorization logic for a given authenticated user.
///
/// The wrapped claims have already been checked against the database by
/// the authentication middleware (status, suspension, token version) and
/// carry the user's *current* role.
#[derive(Debug, Clone)]
pub struct AccessControl(pub Claims);

impl AccessControl {
    /// Returns the user ID directly.
    pub fn user_id(&self) -> Uuid {
        self.0.sub
    }

    pub fn role(&self) -> &Role {
        &self.0.role
    }

    pub fn is_admin(&self) -> bool {
        self.0.role == Role::Admin
    }

    pub fn is_staff(&self) -> bool {
        self.0.role.is_staff()
    }

    /// The staff member behind an impersonation session, if any.
    pub fn impersonator(&self) -> Option<Uuid> {
        self.0.imp
    }

    /// Ensures the user has exactly the specified role.
    pub fn require_role(&self, role: Role) -> Result<(), ApiError> {
        if self.0.role == role {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }

    /// Ensures the user has at least one of the specified roles.
    pub fn require_any_role(&self, roles: &[Role]) -> Result<(), ApiError> {
        if roles.contains(&self.0.role) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }

    /// Ensures the caller is a moderator or an admin.
    pub fn require_staff(&self) -> Result<(), ApiError> {
        self.require_any_role(&[Role::Admin, Role::Moderator])
    }

    /// Ensures the caller is an admin.
    pub fn require_admin(&self) -> Result<(), ApiError> {
        self.require_role(Role::Admin)
    }
}

impl<S> FromRequestParts<S> for AccessControl
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let claims = parts
            .extensions
            .get::<Claims>()
            .cloned()
            .ok_or(ApiError::Unauthorized)?;

        Ok(Self(claims))
    }
}

/// The client IP as seen by the API (first `X-Forwarded-For` hop, then
/// `X-Real-IP`), recorded on audit entries. Best-effort and never used for
/// authorization.
#[derive(Debug, Clone, Default)]
pub struct ClientIp(pub Option<String>);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let forwarded = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        let real_ip = || {
            parts
                .headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };

        let ip = forwarded
            .or_else(real_ip)
            .map(|ip| ip.chars().take(64).collect());

        Ok(Self(ip))
    }
}
