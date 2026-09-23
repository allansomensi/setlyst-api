//! Announcements: the staff console (`/admin/announcements`) and what
//! users see (`/announcements`).

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::{ApiError, codes},
    jobs,
    models::{
        PaginatedResponse, PaginationQuery,
        announcement::{
            AUDIENCE_PLAN_NONE, AUDIENCE_PLAN_TRIAL, ActiveAnnouncements, AdminAnnouncement,
            Announcement, AnnouncementDraft, AnnouncementLevel, AnnouncementListQuery,
            AnnouncementReceipt, AnnouncementStatus, AudiencePayload, AudiencePreview,
            CreateAnnouncementPayload, UpdateAnnouncementPayload, UserAnnouncement,
            normalize_audience,
        },
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        resolve_page,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use tracing::{error, info};
use uuid::Uuid;
use validator::Validate;

fn invalid(message: String) -> ApiError {
    let mut errors = validator::ValidationErrors::new();
    let mut error = validator::ValidationError::new("invalid_announcement");
    error.message = Some(message.into());
    errors.add("announcement", error);
    ApiError::ValidationError(errors)
}

/// Checks a complete draft, including plan codes against the database.
async fn check_draft(state: &AppState, draft: &AnnouncementDraft) -> Result<(), ApiError> {
    draft.check().map_err(invalid)?;
    if let Some(plans) = &draft.audience_plans {
        let known: Vec<String> = state
            .billing_repo
            .list_plans(false)
            .await?
            .into_iter()
            .map(|p| p.code)
            .collect();
        if let Some(unknown) = plans.iter().find(|p| {
            p.as_str() != AUDIENCE_PLAN_NONE
                && p.as_str() != AUDIENCE_PLAN_TRIAL
                && !known.contains(p)
        }) {
            return Err(invalid(format!(
                "Unknown plan '{unknown}' in the audience."
            )));
        }
    }
    Ok(())
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

async fn load(state: &AppState, id: Uuid) -> Result<Announcement, ApiError> {
    state
        .announcement_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn with_stats(
    state: &AppState,
    announcement: Announcement,
) -> Result<AdminAnnouncement, ApiError> {
    let stats = state.announcement_repo.stats(&announcement).await?;
    Ok(AdminAnnouncement {
        announcement,
        stats,
    })
}

// ---------------------------------------------------------------------
// Staff
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/announcements",
    tags = ["Announcements"],
    summary = "List announcements (staff).",
    description = "Filter by computed `status` (`draft`, `scheduled`, `active`, `ended`, `archived`). Each item carries `stats` (accounts targeted right now, and how many saw, dismissed or acknowledged it).",
    params(AnnouncementListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Announcements.", body = PaginatedResponse<AdminAnnouncement>))
)]
pub async fn admin_list(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AnnouncementListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let status = match query.status.as_deref().filter(|s| !s.is_empty()) {
        Some(value) => Some(
            AnnouncementStatus::parse(value)
                .ok_or_else(|| ApiError::BadRequest(format!("Unknown status '{value}'.")))?,
        ),
        None => None,
    };
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let (items, total) = state.announcement_repo.list(status, page, per_page).await?;
    let mut data = Vec::with_capacity(items.len());
    for item in items {
        data.push(with_stats(&state, item).await?);
    }
    Ok(Json(PaginatedResponse::new(data, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/announcements",
    tags = ["Announcements"],
    summary = "Create an announcement draft (staff).",
    description = "Title 3 to 120 characters, text 1 to 5000. `cta_url` is an app path starting with `/` or an `https` URL, and needs `cta_label` (and vice versa). Audience: `audience_roles` ⊆ {user, moderator, admin}, `audience_plans` ⊆ plan codes ∪ {none, trial}, `audience_locales` ⊆ {en, pt-BR, es}; empty or null = everybody. At least one channel; `requires_acknowledgement` needs `show_modal`.",
    request_body = CreateAnnouncementPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Draft created.", body = AdminAnnouncement),
        (status = 400, description = "Invalid announcement."),
    )
)]
pub async fn admin_create(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreateAnnouncementPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let draft = AnnouncementDraft {
        title: payload.title,
        body: payload.body,
        level: payload.level.unwrap_or(AnnouncementLevel::Info),
        show_modal: payload.show_modal,
        show_banner: payload.show_banner,
        send_notification: payload.send_notification.unwrap_or(true),
        send_email: payload.send_email,
        dismissible: payload.dismissible.unwrap_or(true),
        requires_acknowledgement: payload.requires_acknowledgement,
        cta_label: trimmed(payload.cta_label),
        cta_url: trimmed(payload.cta_url),
        audience_roles: normalize_audience(payload.audience_roles),
        audience_plans: normalize_audience(payload.audience_plans),
        audience_locales: normalize_audience(payload.audience_locales),
        starts_at: payload.starts_at,
        ends_at: payload.ends_at,
    };
    check_draft(&state, &draft).await?;
    let created = state
        .announcement_repo
        .create(&draft, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::ANNOUNCEMENT_CREATED)
        .target("announcement", created.id, &created.title)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((
        StatusCode::CREATED,
        Json(with_stats(&state, created).await?),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/announcements/{id}",
    tags = ["Announcements"],
    summary = "Get an announcement (staff).",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Announcement.", body = AdminAnnouncement),
        (status = 404, description = "Not found."),
    )
)]
pub async fn admin_get(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let announcement = load(&state, id).await?;
    Ok(Json(with_stats(&state, announcement).await?))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/announcements/{id}",
    tags = ["Announcements"],
    summary = "Edit an announcement (staff).",
    description = "Drafts and scheduled announcements are fully editable. Active ones accept only `title`, `body`, `cta_label`, `cta_url`, `ends_at`, `show_modal` and `show_banner`; ended and archived ones can't be edited (`ANNOUNCEMENT_LOCKED`). The result is validated as a whole.",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    request_body = UpdateAnnouncementPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Updated.", body = AdminAnnouncement),
        (status = 400, description = "Invalid announcement."),
        (status = 409, description = "Not editable in its current status."),
    )
)]
pub async fn admin_update(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateAnnouncementPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let current = load(&state, id).await?;
    let status = current.compute_status(chrono::Utc::now().naive_utc());
    let locked =
        |message: &str| ApiError::rule(StatusCode::CONFLICT, codes::ANNOUNCEMENT_LOCKED, message);
    match status {
        AnnouncementStatus::Ended | AnnouncementStatus::Archived => {
            return Err(locked("Ended or archived announcements can't be edited."));
        }
        AnnouncementStatus::Active if !payload.only_live_editable_fields() => {
            return Err(locked(
                "While active, only the text, the button, the end and the display options can change.",
            ));
        }
        _ => {}
    }

    let draft = AnnouncementDraft {
        title: payload.title.clone().unwrap_or(current.title.clone()),
        body: payload.body.clone().unwrap_or(current.body.clone()),
        level: payload.level.unwrap_or(current.level),
        show_modal: payload.show_modal.unwrap_or(current.show_modal),
        show_banner: payload.show_banner.unwrap_or(current.show_banner),
        send_notification: payload
            .send_notification
            .unwrap_or(current.send_notification),
        send_email: payload.send_email.unwrap_or(current.send_email),
        dismissible: payload.dismissible.unwrap_or(current.dismissible),
        requires_acknowledgement: payload
            .requires_acknowledgement
            .unwrap_or(current.requires_acknowledgement),
        cta_label: match payload.cta_label.clone() {
            Some(value) => trimmed(value),
            None => current.cta_label.clone(),
        },
        cta_url: match payload.cta_url.clone() {
            Some(value) => trimmed(value),
            None => current.cta_url.clone(),
        },
        audience_roles: match payload.audience_roles.clone() {
            Some(value) => normalize_audience(value),
            None => current.audience_roles.clone(),
        },
        audience_plans: match payload.audience_plans.clone() {
            Some(value) => normalize_audience(value),
            None => current.audience_plans.clone(),
        },
        audience_locales: match payload.audience_locales.clone() {
            Some(value) => normalize_audience(value),
            None => current.audience_locales.clone(),
        },
        starts_at: payload.starts_at.unwrap_or(current.starts_at),
        ends_at: payload.ends_at.unwrap_or(current.ends_at),
    };
    check_draft(&state, &draft).await?;
    let updated = state
        .announcement_repo
        .update(id, &draft, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::ANNOUNCEMENT_UPDATED)
        .target("announcement", id, &updated.title)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(with_stats(&state, updated).await?))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/announcements/{id}",
    tags = ["Announcements"],
    summary = "Delete a draft, or archive a published announcement (staff).",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Deleted or archived."))
)]
pub async fn admin_delete(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let current = load(&state, id).await?;
    if current.published_at.is_none() {
        state.announcement_repo.delete(id).await?;
        AuditEvent::by(&access, actions::ANNOUNCEMENT_DELETED)
            .target("announcement", id, &current.title)
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    } else {
        state
            .announcement_repo
            .archive(id, access.user_id())
            .await?;
        AuditEvent::by(&access, actions::ANNOUNCEMENT_ARCHIVED)
            .target("announcement", id, &current.title)
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/announcements/{id}/publish",
    tags = ["Announcements"],
    summary = "Publish an announcement (staff).",
    description = "Makes it visible from `starts_at` (or now). Notifications and e-mails go out once, as soon as the window starts (immediately when it already has).",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Published.", body = AdminAnnouncement),
        (status = 409, description = "Archived."),
    )
)]
pub async fn admin_publish(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let current = load(&state, id).await?;
    if current.archived_at.is_some() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::ANNOUNCEMENT_LOCKED,
            "Archived announcements can't be published.",
        ));
    }
    let published = state
        .announcement_repo
        .publish(id, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::ANNOUNCEMENT_PUBLISHED)
        .target("announcement", id, &published.title)
        .meta(json!({ "starts_at": published.starts_at }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    if published
        .starts_at
        .is_none_or(|start| start <= chrono::Utc::now().naive_utc())
        && let Err(e) = jobs::announcements::deliver(&state, id).await
    {
        error!(announcement_id = %id, error = %e, "Immediate announcement delivery failed; the job will retry");
    }

    let published = load(&state, id).await?;
    Ok(Json(with_stats(&state, published).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/announcements/{id}/archive",
    tags = ["Announcements"],
    summary = "Archive an announcement (staff).",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Archived.", body = AdminAnnouncement))
)]
pub async fn admin_archive(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    load(&state, id).await?;
    let archived = state
        .announcement_repo
        .archive(id, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::ANNOUNCEMENT_ARCHIVED)
        .target("announcement", id, &archived.title)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(with_stats(&state, archived).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/announcements/preview-audience",
    tags = ["Announcements"],
    summary = "Count the accounts an audience matches (staff).",
    request_body = AudiencePayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Matching accounts.", body = AudiencePreview))
)]
pub async fn admin_preview_audience(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<AudiencePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let roles = normalize_audience(payload.audience_roles);
    let plans = normalize_audience(payload.audience_plans);
    let locales = normalize_audience(payload.audience_locales);
    let count = state
        .announcement_repo
        .count_audience(roles.as_deref(), plans.as_deref(), locales.as_deref())
        .await?;
    Ok(Json(AudiencePreview { count }))
}

// ---------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------

fn to_user(pairs: Vec<(Announcement, AnnouncementReceipt)>) -> Vec<UserAnnouncement> {
    pairs
        .into_iter()
        .map(|(announcement, receipt)| UserAnnouncement {
            announcement,
            receipt,
        })
        .collect()
}

#[utoipa::path(
    get,
    path = "/api/v1/announcements",
    tags = ["Announcements"],
    summary = "Announcements for the current user.",
    description = "Published announcements targeted at the caller whose window started (ended ones included, archived ones not), newest first, each with the caller's `receipt`.",
    params(PaginationQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Announcements.", body = PaginatedResponse<UserAnnouncement>))
)]
pub async fn list_for_user(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (page, per_page) = query.resolve();
    let (items, total) = state
        .announcement_repo
        .list_for_user(access.user_id(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(
        to_user(items),
        total,
        page,
        per_page,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/announcements/active",
    tags = ["Announcements"],
    summary = "Announcements to show now.",
    description = "`modal`: active modal announcements not yet dismissed or acknowledged, oldest first. `banner`: active banner announcements not dismissed.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Active announcements.", body = ActiveAnnouncements))
)]
pub async fn active_for_user(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let items = to_user(
        state
            .announcement_repo
            .active_for_user(access.user_id())
            .await?,
    );
    let modal = items
        .iter()
        .filter(|a| {
            a.announcement.show_modal
                && a.receipt.dismissed_at.is_none()
                && a.receipt.acknowledged_at.is_none()
        })
        .cloned()
        .collect();
    let banner = items
        .into_iter()
        .filter(|a| a.announcement.show_banner && a.receipt.dismissed_at.is_none())
        .collect();
    Ok(Json(ActiveAnnouncements { modal, banner }))
}

async fn visible(
    state: &AppState,
    access: &AccessControl,
    id: Uuid,
) -> Result<Announcement, ApiError> {
    state
        .announcement_repo
        .find_for_user(id, access.user_id())
        .await?
        .map(|(announcement, _)| announcement)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    post,
    path = "/api/v1/announcements/{id}/seen",
    tags = ["Announcements"],
    summary = "Mark an announcement as seen.",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Receipt.", body = AnnouncementReceipt))
)]
pub async fn mark_seen(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    visible(&state, &access, id).await?;
    Ok(Json(
        state
            .announcement_repo
            .mark_receipt(id, access.user_id(), true, false, false)
            .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/announcements/{id}/dismiss",
    tags = ["Announcements"],
    summary = "Dismiss an announcement.",
    description = "Refused (`NOT_DISMISSIBLE`, 409) when it is not dismissible or requires an acknowledgement.",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Receipt.", body = AnnouncementReceipt),
        (status = 409, description = "Not dismissible."),
    )
)]
pub async fn dismiss(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let announcement = visible(&state, &access, id).await?;
    if !announcement.dismissible || announcement.requires_acknowledgement {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::NOT_DISMISSIBLE,
            "This announcement can't be dismissed.",
        ));
    }
    Ok(Json(
        state
            .announcement_repo
            .mark_receipt(id, access.user_id(), true, true, false)
            .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/announcements/{id}/acknowledge",
    tags = ["Announcements"],
    summary = "Acknowledge an announcement.",
    params(("id" = Uuid, Path, description = "Announcement UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Receipt.", body = AnnouncementReceipt))
)]
pub async fn acknowledge(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    visible(&state, &access, id).await?;
    let receipt = state
        .announcement_repo
        .mark_receipt(id, access.user_id(), true, false, true)
        .await?;
    info!(user_id = %access.user_id(), announcement_id = %id, "Announcement acknowledged");
    Ok(Json(receipt))
}
