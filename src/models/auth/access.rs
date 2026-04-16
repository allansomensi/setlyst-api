use crate::{
    errors::api_error::ApiError,
    models::{auth::token::Claims, user::Role},
};
use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

/// Wrapper struct providing authorization logic for a given authenticated user.
#[derive(Debug, Clone)]
pub struct AccessControl(pub Claims);

impl AccessControl {
    /// Returns the user ID directly.
    pub fn user_id(&self) -> Uuid {
        self.0.sub
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
