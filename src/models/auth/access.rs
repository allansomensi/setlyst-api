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

/// Request extension set by the authentication middleware on requests
/// made with an impersonation ("view as") token. Handlers and response
/// filters use it to withhold secrets from staff viewing as someone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Impersonation {
    pub impersonator_id: Uuid,
}

/// What an impersonation response must not carry, replaced in place:
/// every `share_token` (a public link staff could keep using after the
/// session) becomes `null`, and, when `invite_codes`, every `code` (band
/// invite codes, which would let staff join a private band under their own
/// identity) becomes `"***"`. Only call it with `invite_codes` on invite
/// answers: `code` is a common key elsewhere (plan codes, error codes).
pub fn redact_for_impersonation(value: &mut serde_json::Value, invite_codes: bool) {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key == "share_token" {
                    if !item.is_null() {
                        *item = Value::Null;
                    }
                } else if invite_codes && key == "code" && item.is_string() {
                    *item = Value::String("***".into());
                } else {
                    redact_for_impersonation(item, invite_codes);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_for_impersonation(item, invite_codes);
            }
        }
        _ => {}
    }
}

/// The client IP as seen by the API, resolved exactly like the rate
/// limiters do (see `middlewares::client_ip`): forwarding headers only
/// count when they come from a trusted proxy or carry the internal secret.
/// Recorded on audit entries.
#[derive(Debug, Clone, Default)]
pub struct ClientIp(pub Option<String>);

impl ClientIp {
    /// The resolved address, when there is one.
    pub fn addr(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|axum::extract::ConnectInfo(addr)| addr.ip());
        let ip = crate::middlewares::client_ip::resolve_with_config(peer, &parts.headers);
        Ok(Self(ip.map(|ip| ip.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn impersonation_redacts_share_tokens_everywhere_and_invite_codes_on_request() {
        let mut value = json!({
            "share_token": "abc",
            "items": [{ "share_token": "def", "code": "INVITE123" }],
            "code": "pro"
        });
        redact_for_impersonation(&mut value, false);
        assert!(value["share_token"].is_null());
        assert!(value["items"][0]["share_token"].is_null());
        assert_eq!(value["items"][0]["code"], "INVITE123");
        assert_eq!(value["code"], "pro");
        redact_for_impersonation(&mut value, true);
        assert_eq!(value["items"][0]["code"], "***");
    }
}
