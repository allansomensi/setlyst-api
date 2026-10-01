//! Internal staff notes on accounts (`/admin/users/{id}/notes`).
//!
//! Any staff member reads and writes notes on accounts other than their
//! own (nobody keeps notes on themselves, nor reads what colleagues wrote
//! about them); only the author or an admin edits or deletes one.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::{
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        user::UserPublic,
        user_note::{CreateUserNotePayload, UpdateUserNotePayload, UserStaffNote},
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;
use validator::Validate;

/// Notes one account can carry.
pub const MAX_NOTES_PER_USER: i64 = 200;

async fn noted_account(
    state: &AppState,
    access: &AccessControl,
    user_id: Uuid,
) -> Result<UserPublic, ApiError> {
    access.require_staff()?;
    if user_id == access.user_id() {
        return Err(ApiError::cannot_target_self(
            "Staff notes are about other accounts.",
        ));
    }
    state
        .user_repo
        .find_by_id(user_id)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn editable_note(
    state: &AppState,
    access: &AccessControl,
    user_id: Uuid,
    note_id: Uuid,
) -> Result<(UserPublic, UserStaffNote), ApiError> {
    let user = noted_account(state, access, user_id).await?;
    let note = state
        .user_note_repo
        .find(user_id, note_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if note.author_id != Some(access.user_id()) && !access.is_admin() {
        return Err(ApiError::insufficient_role(
            "Only the author or an admin can change this note.",
        ));
    }
    Ok((user, note))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/users/{id}/notes",
    tags = ["Admin"],
    summary = "Internal notes on an account, pinned first (staff).",
    params(("id" = Uuid, Path, description = "User UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Notes.", body = [UserStaffNote]))
)]
pub async fn list_notes(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    noted_account(&state, &access, id).await?;
    Ok(Json(state.user_note_repo.list(id).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/users/{id}/notes",
    tags = ["Admin"],
    summary = "Leave an internal note on an account (staff).",
    description = "`body` 1 to 2 000 characters. Never shown to the account, but included in its personal data export (without the author). At most 200 notes per account.",
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = CreateUserNotePayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Created.", body = UserStaffNote))
)]
pub async fn create_note(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<CreateUserNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let user = noted_account(&state, &access, id).await?;
    if state.user_note_repo.count(id).await? >= MAX_NOTES_PER_USER {
        return Err(ApiError::BadRequest(format!(
            "An account can carry at most {MAX_NOTES_PER_USER} notes. Delete old ones first."
        )));
    }
    let note = state
        .user_note_repo
        .create(
            id,
            access.user_id(),
            &access.0.username,
            &payload.body,
            payload.pinned,
        )
        .await?;
    AuditEvent::by(&access, actions::USER_NOTE_CREATED)
        .target("user", id, &user.username)
        .meta(json!({ "note_id": note.id, "pinned": note.pinned }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::CREATED, Json(note)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/users/{id}/notes/{note_id}",
    tags = ["Admin"],
    summary = "Edit or pin a note (its author, or an admin).",
    params(
        ("id" = Uuid, Path, description = "User UUID"),
        ("note_id" = Uuid, Path, description = "Note UUID"),
    ),
    request_body = UpdateUserNotePayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = UserStaffNote))
)]
pub async fn update_note(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path((id, note_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateUserNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    let (user, _) = editable_note(&state, &access, id, note_id).await?;
    let note = state
        .user_note_repo
        .update(note_id, payload.body.as_deref(), payload.pinned)
        .await?;
    AuditEvent::by(&access, actions::USER_NOTE_UPDATED)
        .target("user", id, &user.username)
        .meta(
            json!({ "note_id": note_id, "pinned": note.pinned, "edited": payload.body.is_some() }),
        )
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(note))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/users/{id}/notes/{note_id}",
    tags = ["Admin"],
    summary = "Delete a note (its author, or an admin).",
    params(
        ("id" = Uuid, Path, description = "User UUID"),
        ("note_id" = Uuid, Path, description = "Note UUID"),
    ),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Deleted."))
)]
pub async fn delete_note(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path((id, note_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let (user, _) = editable_note(&state, &access, id, note_id).await?;
    state.user_note_repo.delete(note_id).await?;
    AuditEvent::by(&access, actions::USER_NOTE_DELETED)
        .target("user", id, &user.username)
        .meta(json!({ "note_id": note_id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(StatusCode::NO_CONTENT)
}
