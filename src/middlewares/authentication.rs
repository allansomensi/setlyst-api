use crate::{
    database::{
        AppState,
        repositories::{audit_repository::AuditEvent, user_repository::AuthState},
    },
    errors::{api_error::ApiError, auth_error::AuthError},
    models::{
        audit::actions,
        auth::{
            access::{ClientIp, Impersonation, redact_for_impersonation},
            token::Claims,
        },
        user::{Status, active_ban},
    },
    utils::jwt::decode_jwt,
};
use axum::{
    body::Body,
    extract::{FromRequestParts, Request, State},
    http::{self, HeaderValue, Method, Response, header},
    middleware::Next,
};
use chrono::{Duration, NaiveDateTime, Utc};
use serde_json::json;
use tracing::error;

/// Staff accounts must enable two-factor authentication: after this long
/// in a staff role without it, everything but their own account settings
/// answers `STAFF_TWO_FACTOR_REQUIRED` (the time to sign in and enrol).
pub const STAFF_TWO_FACTOR_GRACE_MINUTES: i64 = 60;

/// Largest JSON answer rewritten for an impersonation session; larger
/// ones are refused rather than passed through unredacted.
const MAX_REDACTED_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The part of `path` after `/users/me`, when `path` is `/users/me` or
/// below it (with or without the `/api/v1` prefix).
fn users_me_suffix(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    let rest = path
        .strip_prefix("/api/v1")
        .unwrap_or(path)
        .strip_prefix("/users/me")?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// Endpoints a staff account without two-factor authentication may still
/// call (the web sends it to the security settings): reading its own
/// account, everything about 2FA, the e-mail flows, re-authentication
/// codes, accepting the terms and changing the password.
fn allowed_without_staff_two_factor(method: &Method, path: &str) -> bool {
    let Some(rest) = users_me_suffix(path) else {
        return false;
    };
    (method == Method::GET)
        || rest == "/2fa"
        || rest.starts_with("/2fa/")
        || rest.starts_with("/email/")
        || rest == "/reauth/code"
        || (method == Method::POST && rest == "/accept-terms")
        || (method == Method::PATCH && rest == "/password")
}

/// `true` when a staff account has had its role long enough without
/// enabling two-factor authentication.
fn staff_two_factor_missing(account: &AuthState, now: NaiveDateTime) -> bool {
    account.role.is_staff()
        && !account.two_factor_enabled
        && account.role_since + Duration::minutes(STAFF_TWO_FACTOR_GRACE_MINUTES) <= now
}

/// Bulk exports refused to impersonation sessions: staff "viewing as"
/// someone must not walk away with their whole repertoire or data.
fn is_bulk_export(method: &Method, path: &str) -> bool {
    if method != Method::GET && method != Method::POST {
        return false;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    path == "/backup/export"
        || path == "/songs/export/chordpro"
        || path == "/users/me/data-export"
        || (path.starts_with("/setlists/") && path.ends_with("/export/pdf"))
}

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

            // Staff who haven't enabled 2FA can't open "view as" sessions,
            // and the ones they opened stop working.
            if staff_two_factor_missing(&impersonator, Utc::now().naive_utc()) {
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

            if let Some((method, path)) = request
                && staff_two_factor_missing(&account, Utc::now().naive_utc())
                && !allowed_without_staff_two_factor(method, path)
            {
                return Err(ApiError::staff_two_factor_required());
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

    let impersonation = claims
        .imp
        .map(|impersonator_id| Impersonation { impersonator_id });
    let target = (claims.sub, claims.username.clone());
    req.extensions_mut().insert(claims);

    let Some(impersonation) = impersonation else {
        return Ok(next.run(req).await);
    };
    req.extensions_mut().insert(impersonation);

    // Every request of a "view as" session is recorded (LGPD
    // accountability for staff access to personal data), bulk exports are
    // refused, and secrets are withheld from the answers.
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let blocked = is_bulk_export(&method, &path);
    let (mut parts, body) = req.into_parts();
    let ip = ClientIp::from_request_parts(&mut parts, &state)
        .await
        .unwrap_or_default();
    let req = Request::from_parts(parts, body);
    let mut event = AuditEvent::new(actions::USER_IMPERSONATED_READ)
        .target("user", target.0, &target.1)
        .meta(json!({ "method": method.as_str(), "path": path, "blocked": blocked }))
        .ip(&ip.0);
    event.actor_id = Some(impersonation.impersonator_id);
    event.impersonator_id = Some(impersonation.impersonator_id);
    event.spawn(state.audit_repo.clone());
    if blocked {
        return Err(ApiError::impersonation_read_only());
    }

    let response = next.run(req).await;
    redact_impersonation_response(response, path.contains("/invites")).await
}

/// Rewrites a JSON answer served to an impersonation session through
/// [`redact_for_impersonation`] (share tokens, and invite codes on invite
/// answers). Anything that isn't a successful JSON answer passes through.
async fn redact_impersonation_response(
    response: Response<Body>,
    invite_codes: bool,
) -> Result<Response<Body>, ApiError> {
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !is_json || !response.status().is_success() {
        return Ok(response);
    }
    let (mut parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_REDACTED_BODY_BYTES)
        .await
        .map_err(|e| {
            error!(error = %e, "Could not read an answer to redact for impersonation");
            ApiError::ServerError(axum::Error::new("unreadable response body"))
        })?;
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(Response::from_parts(parts, Body::from(bytes)));
    };
    redact_for_impersonation(&mut value, invite_codes);
    let rewritten =
        serde_json::to_vec(&value).map_err(|e| ApiError::ServerError(axum::Error::new(e)))?;
    parts.headers.remove(header::CONTENT_LENGTH);
    if let Ok(length) = HeaderValue::from_str(&rewritten.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, length);
    }
    Ok(Response::from_parts(parts, Body::from(rewritten)))
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
    fn staff_without_two_factor_keep_their_account_settings() {
        for (method, path) in [
            (Method::GET, "/users/me"),
            (Method::GET, "/api/v1/users/me/security"),
            (Method::POST, "/users/me/2fa/setup"),
            (Method::POST, "/users/me/2fa/enable"),
            (Method::POST, "/users/me/accept-terms"),
            (Method::PATCH, "/users/me/password"),
            (Method::POST, "/users/me/reauth/code"),
            (Method::POST, "/users/me/email/verify"),
        ] {
            assert!(
                allowed_without_staff_two_factor(&method, path),
                "{method} {path}"
            );
        }
        for (method, path) in [
            (Method::GET, "/users"),
            (Method::GET, "/admin/audit-logs"),
            (Method::PATCH, "/users/me"),
            (Method::DELETE, "/users/me"),
            (Method::POST, "/users/meow"),
            (Method::GET, "/users/mean/profile"),
        ] {
            assert!(
                !allowed_without_staff_two_factor(&method, path),
                "{method} {path}"
            );
        }
    }

    #[test]
    fn bulk_exports_are_recognized() {
        assert!(is_bulk_export(&Method::GET, "/backup/export"));
        assert!(is_bulk_export(
            &Method::GET,
            "/api/v1/songs/export/chordpro"
        ));
        assert!(is_bulk_export(&Method::GET, "/users/me/data-export"));
        assert!(is_bulk_export(&Method::POST, "/setlists/abc/export/pdf"));
        assert!(!is_bulk_export(&Method::GET, "/songs/abc/export/chordpro"));
        assert!(!is_bulk_export(&Method::GET, "/setlists/abc"));
    }

    #[test]
    fn impersonation_is_read_only() {
        assert!(is_read_only(&Method::GET));
        assert!(!is_read_only(&Method::POST));
        assert!(!is_read_only(&Method::PATCH));
        assert!(!is_read_only(&Method::DELETE));
    }
}
