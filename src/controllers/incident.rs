//! Incidents and scheduled maintenance on the status page: published by
//! staff (`/admin/incidents`), read by anyone (`/public/incidents`).

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::{
        PaginatedResponse,
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        incident::{
            CreateIncidentPayload, Incident, IncidentKind, IncidentListQuery, IncidentStatus,
            PostIncidentUpdatePayload, PublicIncidents, UpdateIncidentPayload,
        },
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use chrono::{Duration, Utc};
use serde_json::json;
use tracing::info;
use uuid::Uuid;
use validator::Validate;

/// Resolved incidents stay on the public page this long.
pub const PUBLIC_INCIDENT_DAYS: i64 = 30;

async fn load(state: &AppState, id: Uuid) -> Result<Incident, ApiError> {
    state
        .incident_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)
}

fn check_window(
    kind: IncidentKind,
    from: Option<chrono::NaiveDateTime>,
    until: Option<chrono::NaiveDateTime>,
) -> Result<(), ApiError> {
    if kind == IncidentKind::Maintenance && (from.is_none() || until.is_none()) {
        return Err(ApiError::BadRequest(
            "Maintenance needs a window (`scheduled_for` and `scheduled_until`).".into(),
        ));
    }
    if let (Some(from), Some(until)) = (from, until)
        && until <= from
    {
        return Err(ApiError::BadRequest(
            "The window must end after it starts.".into(),
        ));
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "/api/v1/public/incidents",
    tags = ["Public"],
    summary = "Incidents and maintenance for the status page.",
    description = "`active`: everything not resolved yet (ongoing incidents and upcoming or ongoing maintenance). `recent`: resolved in the last 30 days. Both newest first, each with its timeline (newest update first). Cached for 30 seconds.",
    responses((status = 200, description = "Incidents.", body = PublicIncidents))
)]
pub async fn public_incidents(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let since = Utc::now().naive_utc() - Duration::days(PUBLIC_INCIDENT_DAYS);
    let (active, recent) = state.incident_repo.public(since).await?;
    Ok((
        [(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=30"),
        )],
        Json(PublicIncidents { active, recent }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/incidents",
    tags = ["Admin"],
    summary = "Every incident, unresolved first (staff).",
    params(IncidentListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Incidents.", body = PaginatedResponse<Incident>))
)]
pub async fn list(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<IncidentListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = crate::models::resolve_page(query.page, query.per_page, 20);
    let (incidents, total) = state
        .incident_repo
        .list(query.active.unwrap_or(false), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(
        incidents, total, page, per_page,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/incidents/{id}",
    tags = ["Admin"],
    summary = "An incident with its timeline (staff).",
    params(("id" = Uuid, Path, description = "Incident UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Incident.", body = Incident))
)]
pub async fn get(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(load(&state, id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/incidents",
    tags = ["Admin"],
    summary = "Publish an incident or schedule maintenance (staff).",
    description = "Published on the public status page at once. `kind`: `incident` or `maintenance` (which needs `scheduled_for` and `scheduled_until`, and starts as `scheduled`). `impact`: `none`, `minor`, `major` or `critical`. `components` among `web`, `api`, `sync`, `email`, `payments`, `exports`. `message` is the first entry of the timeline.",
    request_body = CreateIncidentPayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Published.", body = Incident))
)]
pub async fn create(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreateIncidentPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    check_window(payload.kind, payload.scheduled_for, payload.scheduled_until)?;
    let status = payload.status.unwrap_or(match payload.kind {
        IncidentKind::Maintenance => IncidentStatus::Scheduled,
        IncidentKind::Incident => IncidentStatus::Investigating,
    });
    if payload.kind == IncidentKind::Incident && status == IncidentStatus::Scheduled {
        return Err(ApiError::BadRequest(
            "Only maintenance can be scheduled.".into(),
        ));
    }
    let incident = state
        .incident_repo
        .create(&payload, status, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::INCIDENT_CREATED)
        .target("incident", incident.incident.id, &incident.incident.title)
        .meta(json!({ "kind": payload.kind, "impact": payload.impact, "status": status }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(incident = %incident.incident.id, kind = ?payload.kind, "Status incident published");
    Ok((StatusCode::CREATED, Json(incident)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/incidents/{id}",
    tags = ["Admin"],
    summary = "Edit an incident's title, impact, components or window (staff).",
    params(("id" = Uuid, Path, description = "Incident UUID")),
    request_body = UpdateIncidentPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = Incident))
)]
pub async fn update(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateIncidentPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let current = load(&state, id).await?.incident;
    check_window(
        current.kind,
        payload.scheduled_for.unwrap_or(current.scheduled_for),
        payload.scheduled_until.unwrap_or(current.scheduled_until),
    )?;
    state.incident_repo.update(id, &payload).await?;
    let incident = load(&state, id).await?;
    AuditEvent::by(&access, actions::INCIDENT_UPDATED)
        .target("incident", id, &incident.incident.title)
        .meta(json!({ "edited": true }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(incident))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/incidents/{id}/updates",
    tags = ["Admin"],
    summary = "Post a timeline update, moving the incident to its status (staff).",
    description = "`resolved` closes it (it stays on the public page for 30 days).",
    params(("id" = Uuid, Path, description = "Incident UUID")),
    request_body = PostIncidentUpdatePayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Posted.", body = Incident))
)]
pub async fn post_update(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<PostIncidentUpdatePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let current = load(&state, id).await?.incident;
    if current.kind == IncidentKind::Incident && payload.status == IncidentStatus::Scheduled {
        return Err(ApiError::BadRequest(
            "Only maintenance can be scheduled.".into(),
        ));
    }
    state
        .incident_repo
        .post_update(id, payload.status, &payload.body, access.user_id())
        .await?;
    let incident = load(&state, id).await?;
    AuditEvent::by(&access, actions::INCIDENT_UPDATED)
        .target("incident", id, &incident.incident.title)
        .meta(json!({ "status": payload.status, "previous_status": current.status }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::CREATED, Json(incident)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/incidents/{id}",
    tags = ["Admin"],
    summary = "Delete an incident published by mistake (admin).",
    params(("id" = Uuid, Path, description = "Incident UUID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Deleted."))
)]
pub async fn delete(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let incident = load(&state, id).await?;
    state.incident_repo.delete(id).await?;
    AuditEvent::by(&access, actions::INCIDENT_DELETED)
        .target("incident", id, &incident.incident.title)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(StatusCode::NO_CONTENT)
}
