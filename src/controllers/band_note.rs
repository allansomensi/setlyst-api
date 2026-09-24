//! Band reminders (`/bands/{id}/notes`).

use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        band::BandRole,
        band_note::{
            BandNote, BandNoteRow, CreateBandNotePayload, MAX_BAND_NOTES, UpdateBandNotePayload,
        },
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::info;
use uuid::Uuid;
use validator::Validate;

/// Its author, or a moderator or above acting on a note whose author ranks
/// below them (or has left the band); the owner may edit any note.
fn can_edit(role: BandRole, row: &BandNoteRow, user_id: Uuid) -> bool {
    if row.author_id == Some(user_id) {
        return true;
    }
    role.satisfies(BandRole::Moderator)
        && (role == BandRole::Owner || row.author_role.is_none_or(|author| author < role))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/notes",
    tags = ["Bands"],
    summary = "A band's reminders.",
    description = "Any member. Pinned first, then newest. `can_edit` tells whether the caller may change each one (its author, or a moderator or above ranking higher than the author; the owner always).",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Reminders.", body = [BandNote]))
)]
pub async fn list_notes(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let role = state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let notes: Vec<BandNote> = state
        .band_note_repo
        .list(band_id)
        .await?
        .into_iter()
        .map(|row| {
            let editable = can_edit(role, &row, user_id);
            row.into_note(editable)
        })
        .collect();
    Ok(Json(notes))
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/notes",
    tags = ["Bands"],
    summary = "Add a reminder.",
    description = "Any member; pinning requires the `moderator` role or above. At most 100 per band (`QUOTA_EXCEEDED`, `meta: {resource: \"band_notes\", limit: 100}`).",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = CreateBandNotePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Reminder created.", body = BandNote),
        (status = 403, description = "Pinning without the role, or the band is full.")
    )
)]
pub async fn create_note(
    State(state): State<AppState>,
    access: AccessControl,
    Path(band_id): Path<Uuid>,
    Json(payload): Json<CreateBandNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    let role = state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    if payload.is_pinned == Some(true) && !role.satisfies(BandRole::Moderator) {
        return Err(ApiError::Forbidden);
    }
    let id = state
        .band_note_repo
        .create(band_id, user_id, &payload, MAX_BAND_NOTES)
        .await?
        .ok_or_else(|| ApiError::quota_exceeded("band_notes", MAX_BAND_NOTES))?;
    info!(%user_id, %band_id, note_id = %id, "Band reminder created");
    let row = state
        .band_note_repo
        .find(id, band_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(row.into_note(true))))
}

#[utoipa::path(
    patch,
    path = "/api/v1/bands/{id}/notes/{nid}",
    tags = ["Bands"],
    summary = "Edit a reminder.",
    description = "Its author, or a moderator or above. Changing `is_pinned` requires the `moderator` role or above. `due_at: null` clears the date.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("nid" = Uuid, Path, description = "The reminder ID")
    ),
    request_body = UpdateBandNotePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Updated reminder.", body = BandNote),
        (status = 403, description = "Not allowed."),
        (status = 404, description = "Not found.")
    )
)]
pub async fn update_note(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateBandNotePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    payload.validate()?;
    let role = state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let row = state
        .band_note_repo
        .find(id, band_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !can_edit(role, &row, user_id)
        || (payload.is_pinned.is_some() && !role.satisfies(BandRole::Moderator))
    {
        return Err(ApiError::Forbidden);
    }
    state
        .band_note_repo
        .update(id, band_id, &payload, user_id)
        .await?;
    let row = state
        .band_note_repo
        .find(id, band_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(row.into_note(true)))
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}/notes/{nid}",
    tags = ["Bands"],
    summary = "Delete a reminder.",
    description = "Its author, or a moderator or above whose role is higher than the author's (the owner may delete any reminder).",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("nid" = Uuid, Path, description = "The reminder ID")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Deleted."),
        (status = 403, description = "Not allowed."),
        (status = 404, description = "Not found.")
    )
)]
pub async fn delete_note(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let role = state
        .band_repo
        .require_role(band_id, user_id, BandRole::Member)
        .await?;
    let row = state
        .band_note_repo
        .find(id, band_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !can_edit(role, &row, user_id) {
        return Err(ApiError::Forbidden);
    }
    state.band_note_repo.delete(id, band_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
