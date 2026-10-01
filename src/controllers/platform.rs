//! Platform switches: maintenance mode, sign-ups and blocked e-mail
//! domains (`/admin/settings/platform`), and what anyone may read about
//! them (`/public/platform`).

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::{
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        platform::{
            MaintenanceMode, PlatformSettings, PublicPlatformStatus, UpdatePlatformSettings,
        },
    },
};
use axum::{Json, extract::State, response::IntoResponse};
use chrono::Utc;
use serde_json::json;
use tracing::warn;
use validator::Validate;

#[utoipa::path(
    get,
    path = "/api/v1/public/platform",
    tags = ["Public"],
    summary = "Maintenance mode and whether sign-ups are open.",
    description = "Clients poll it to show a maintenance banner (`read_only`) or page (`full`) and to hide the sign-up form while `registrations_open` is `false`. Cached for up to 5 seconds per server.",
    responses((status = 200, description = "Platform state.", body = PublicPlatformStatus))
)]
pub async fn public_status(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.platform_repo.get().await?.public()))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/settings/platform",
    tags = ["Admin"],
    summary = "Platform switches (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Settings.", body = PlatformSettings))
)]
pub async fn get_settings(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.platform_repo.load().await?))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/settings/platform",
    tags = ["Admin"],
    summary = "Change the platform switches (admin).",
    description = "Maintenance `mode`: `off`, `read_only` (everyone can read, changes answer `MAINTENANCE_MODE`) or `full` (only staff can sign in or use the API). `started_at` is set by the API when the mode changes. `registrations_open: false` closes sign-ups (`REGISTRATION_CLOSED`); staff can still create accounts. `blocked_email_domains` (at most 500; subdomains included) can't be used by new addresses (`EMAIL_DOMAIN_BLOCKED`). Every switch must be sent (unknown fields are refused): a partial body would reset the others. Applies at once on this server and within 5 seconds on the others.",
    request_body = UpdatePlatformSettings,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Saved.", body = PlatformSettings))
)]
pub async fn update_settings(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<UpdatePlatformSettings>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let previous = state.platform_repo.load().await?;
    let mut settings = payload.into_settings().normalized();
    settings.maintenance.started_at = match settings.maintenance.mode {
        MaintenanceMode::Off => None,
        mode if mode == previous.maintenance.mode => previous.maintenance.started_at,
        _ => Some(Utc::now().naive_utc()),
    };
    state
        .platform_repo
        .save(&settings, access.user_id())
        .await?;

    if settings.maintenance.mode != previous.maintenance.mode {
        warn!(
            from = ?previous.maintenance.mode,
            to = ?settings.maintenance.mode,
            by = %access.user_id(),
            "Maintenance mode changed"
        );
    }
    AuditEvent::by(&access, actions::PLATFORM_SETTINGS_UPDATED)
        .meta(json!({
            "maintenance_mode": settings.maintenance.mode,
            "previous_maintenance_mode": previous.maintenance.mode,
            "registrations_open": settings.registrations_open,
            "blocked_email_domains": settings.blocked_email_domains.len(),
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(settings))
}
