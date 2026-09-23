//! Release notes in the staff console (`/admin/release-notes`). The public
//! list lives in `controllers::public`.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    email::{EmailTemplate, OutgoingEmail, outbox::enqueue_many, templates::ReleaseItem},
    errors::api_error::ApiError,
    models::{
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        release_note::{
            CreateReleaseNotePayload, PublishReleaseNotePayload, ReleaseNote,
            UpdateReleaseNotePayload,
        },
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use tracing::info;
use uuid::Uuid;
use validator::Validate;

async fn load(state: &AppState, id: Uuid) -> Result<ReleaseNote, ApiError> {
    state
        .release_note_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/release-notes",
    tags = ["Release notes"],
    summary = "Every release note, drafts included (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Release notes.", body = [ReleaseNote]))
)]
pub async fn admin_list(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.release_note_repo.list_all().await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/release-notes/{id}",
    tags = ["Release notes"],
    summary = "A release note (staff).",
    params(("id" = Uuid, Path, description = "Release note UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Release note.", body = ReleaseNote))
)]
pub async fn admin_get(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(load(&state, id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/release-notes",
    tags = ["Release notes"],
    summary = "Create a release note draft (admin).",
    description = "`version` like `1.2.3` or `1.2.3-beta.1` (unique, `ALREADY_EXISTS`). `title` and every item's `text` are `{en, pt-BR, es}` maps (`en` and `pt-BR` required; `es` falls back to `en`); titles 3 to 120 characters, texts 3 to 600. 1 to 40 items of kind `new`, `improved`, `fixed` or `security`.",
    request_body = CreateReleaseNotePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Created.", body = ReleaseNote),
        (status = 409, description = "Version already exists."),
    )
)]
pub async fn admin_create(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreateReleaseNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let note = state
        .release_note_repo
        .create(&payload, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::RELEASE_NOTE_CREATED)
        .target("release_note", note.id, &note.version)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::CREATED, Json(note)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/release-notes/{id}",
    tags = ["Release notes"],
    summary = "Edit a release note (admin).",
    description = "Published notes can be edited too; they then show as edited (`is_edited`).",
    params(("id" = Uuid, Path, description = "Release note UUID")),
    request_body = UpdateReleaseNotePayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = ReleaseNote))
)]
pub async fn admin_update(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateReleaseNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let note = state
        .release_note_repo
        .update(id, &payload, access.user_id())
        .await?
        .ok_or(ApiError::NotFound)?;
    AuditEvent::by(&access, actions::RELEASE_NOTE_UPDATED)
        .target("release_note", id, &note.version)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(note))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/release-notes/{id}",
    tags = ["Release notes"],
    summary = "Delete a release note (admin).",
    params(("id" = Uuid, Path, description = "Release note UUID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Deleted."))
)]
pub async fn admin_delete(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let note = load(&state, id).await?;
    state.release_note_repo.delete(id).await?;
    AuditEvent::by(&access, actions::RELEASE_NOTE_DELETED)
        .target("release_note", id, &note.version)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/release-notes/{id}/publish",
    tags = ["Release notes"],
    summary = "Publish a release note (admin).",
    description = "With `notify: true`, every account gets a `release_published` notification (unless it turned product-update notifications off) and accounts that opted in to product-update e-mails get the notes by e-mail. Notifying happens only on the first publication.",
    params(("id" = Uuid, Path, description = "Release note UUID")),
    request_body = PublishReleaseNotePayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Published.", body = ReleaseNote))
)]
pub async fn admin_publish(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<PublishReleaseNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    load(&state, id).await?;
    let was_draft = state
        .release_note_repo
        .set_published(id, true, access.user_id())
        .await?;
    let note = load(&state, id).await?;

    let mut notified = 0;
    let mut emailed = 0;
    if payload.notify && was_draft {
        let (created, recipients) = state.release_note_repo.fan_out(&note).await?;
        notified = created;
        let items: Vec<ReleaseItem> = note
            .items
            .iter()
            .map(|item| ReleaseItem {
                kind: item.kind.clone(),
                text: item.text.clone(),
            })
            .collect();
        let emails: Vec<OutgoingEmail> = recipients
            .into_iter()
            .map(|r| OutgoingEmail {
                user_id: Some(r.user_id),
                to: r.email,
                locale: r.language,
                template: EmailTemplate::ReleaseNotes {
                    version: note.version.clone(),
                    title: note.title.clone(),
                    items: items.clone(),
                    released_on: note.released_on,
                },
            })
            .collect();
        emailed = enqueue_many(&state.db, &emails).await?;
    }

    AuditEvent::by(&access, actions::RELEASE_NOTE_PUBLISHED)
        .target("release_note", id, &note.version)
        .meta(json!({ "notify": payload.notify, "notified": notified, "emailed": emailed }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(release = %note.version, notified, emailed, "Release note published");
    Ok(Json(note))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/release-notes/{id}/unpublish",
    tags = ["Release notes"],
    summary = "Return a release note to draft (admin).",
    params(("id" = Uuid, Path, description = "Release note UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Unpublished.", body = ReleaseNote))
)]
pub async fn admin_unpublish(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    load(&state, id).await?;
    state
        .release_note_repo
        .set_published(id, false, access.user_id())
        .await?;
    let note = load(&state, id).await?;
    AuditEvent::by(&access, actions::RELEASE_NOTE_UNPUBLISHED)
        .target("release_note", id, &note.version)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(note))
}
