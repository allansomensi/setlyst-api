use crate::{
    errors::api_error::ApiError,
    models::user::{Role, Status, User},
};
use axum::{extract::FromRequestParts, http::request::Parts};

/// Wrapper struct providing authorization logic for a given authenticated user.
#[derive(Debug, Clone)]
pub struct AccessControl(pub User);

impl AccessControl {
    /// Returns a reference to the inner user.
    pub fn user(&self) -> &User {
        &self.0
    }

    /// Ensures the user has exactly the specified role.
    pub fn require_role(&self, role: Role) -> Result<(), ApiError> {
        if self.0.role == role {
            Ok(())
        } else {
            Err(ApiError::Unauthorized)
        }
    }

    /// Ensures the user has at least one of the specified roles.
    pub fn require_any_role(&self, roles: &[Role]) -> Result<(), ApiError> {
        if roles.contains(&self.0.role) {
            Ok(())
        } else {
            Err(ApiError::Unauthorized)
        }
    }
}

impl<S> FromRequestParts<S> for AccessControl
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    /// Extracts the [`AccessControl`] from the request parts.
    ///
    /// It expects a [`User`] to be present in the request extensions (injected by [crate::middlewares::authentication] middleware).
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let user = parts
            .extensions
            .get::<User>()
            .cloned()
            .ok_or(ApiError::Unauthorized)?;

        if user.status != Status::Active {
            return Err(ApiError::Unauthorized);
        }

        Ok(Self(user))
    }
}
