use crate::{
    database::{AppState, repositories::user_repository::AuthState},
    errors::{api_error::ApiError, auth_error::AuthError},
    models::{
        auth::token::Claims,
        user::{Status, active_ban},
    },
    utils::jwt::decode_jwt,
};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{self, Method, Response},
    middleware::Next,
};
use chrono::Utc;

/// Endpoints an account flagged with `must_change_password` may still
/// call: reading itself (so the client can render the change screen),
/// accepting the current terms and changing the password. Everything else
/// is refused until then.
fn allowed_while_password_change_required(method: &Method, path: &str) -> bool {
    let path = path.trim_end_matches('/');
    (method == Method::GET
        && (path.ends_with("/users/me")
            || path.ends_with("/users/me/preferences")
            || path.ends_with("/users/me/security")))
        || (method == Method::PATCH && path.ends_with("/users/me/password"))
        || (method == Method::POST && path.ends_with("/users/me/accept-terms"))
}

fn is_read_only(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Rejects an account that can't currently use the platform.
fn ensure_account_usable(state: &AuthState) -> Result<(), ApiError> {
    if state.status != Status::Active {
        return Err(ApiError::account_deactivated());
    }
    if let Some(until) = active_ban(state.banned_at, state.banned_until, Utc::now().naive_utc()) {
        return Err(ApiError::account_banned(until, state.ban_reason.clone()));
    }
    Ok(())
}

/// Checks decoded `claims` against the accounts' *current* state and
/// returns the subject's state. Shared by the middleware and
/// `/auth/verify`, so a token is valid in exactly the same cases in both.
///
/// `request` is the method and path of the call being authorized, or
/// `None` when only the token itself is being verified.
pub async fn check_claims(
    state: &AppState,
    claims: &Claims,
    request: Option<(&Method, &str)>,
) -> Result<AuthState, ApiError> {
    let account = state
        .user_repo
        .auth_state(claims.sub)
        .await?
        .ok_or_else(ApiError::session_revoked)?;

    match claims.imp {
        Some(impersonator_id) => {
            let impersonator = state
                .user_repo
                .auth_state(impersonator_id)
                .await?
                .ok_or_else(ApiError::session_revoked)?;

            ensure_account_usable(&impersonator).map_err(|_| ApiError::session_revoked())?;

            // Bound to the impersonator's sessions: signing them out (or a
            // password change) ends every "view as" session they opened.
            if claims.iver != Some(impersonator.token_version) {
                return Err(ApiError::session_revoked());
            }

            if !impersonator.role.outranks(&account.role) {
                return Err(ApiError::session_revoked());
            }

            if let Some((method, _)) = request
                && !is_read_only(method)
            {
                return Err(ApiError::impersonation_read_only());
            }
            // The target's own suspension/deactivation is deliberately
            // *not* enforced here: staff often need to look at exactly
            // those accounts.
        }
        None => {
            if account.token_version != claims.ver {
                return Err(ApiError::session_revoked());
            }

            ensure_account_usable(&account)?;

            if let Some((method, path)) = request
                && account.must_change_password
                && !allowed_while_password_change_required(method, path)
            {
                return Err(ApiError::password_change_required());
            }
        }
    }

    Ok(account)
}

/// Authenticates every request under the protected routes.
///
/// A valid signature is not enough: the token is re-checked against the
/// account's *current* state on every request, so that
///
/// - a deactivated or suspended account is locked out immediately, not
///   when its token happens to expire;
/// - a password change, admin reset or "sign out everywhere" revokes
///   existing tokens (via `token_version`);
/// - role changes apply immediately (the claims are rewritten with the
///   current role before handlers see them);
/// - impersonation tokens stay valid only while the staff member behind
///   them still outranks the target and hasn't been signed out, and can
///   never write.
pub async fn authenticate(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response<Body>, ApiError> {
    let token = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|header_str| header_str.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(ApiError::from(AuthError::MissingToken))?;

    let mut claims = decode_jwt(token.to_string())
        .map_err(|_| ApiError::from(AuthError::InvalidToken))?
        .claims;

    let account = check_claims(&state, &claims, Some((req.method(), req.uri().path()))).await?;

    claims.role = account.role;
    claims.username = account.username;
    claims.status = account.status;

    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_change_allowlist() {
        assert!(allowed_while_password_change_required(
            &Method::GET,
            "/users/me"
        ));
        assert!(allowed_while_password_change_required(
            &Method::GET,
            "/api/v1/users/me/"
        ));
        assert!(allowed_while_password_change_required(
            &Method::PATCH,
            "/users/me/password"
        ));
        assert!(!allowed_while_password_change_required(
            &Method::PATCH,
            "/users/me"
        ));
        assert!(!allowed_while_password_change_required(
            &Method::GET,
            "/songs"
        ));
        assert!(!allowed_while_password_change_required(
            &Method::DELETE,
            "/users/me/password"
        ));
    }

    #[test]
    fn impersonation_is_read_only() {
        assert!(is_read_only(&Method::GET));
        assert!(!is_read_only(&Method::POST));
        assert!(!is_read_only(&Method::PATCH));
        assert!(!is_read_only(&Method::DELETE));
    }
}
