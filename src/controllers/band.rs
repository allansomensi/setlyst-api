use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        band::{
            Band, BandRole, CreateBandInvitePayload, CreateBandPayload, TransferOwnershipPayload,
            UpdateBandMemberRolePayload, UpdateBandPayload,
        },
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
use chrono::Utc;
use tracing::{debug, error, info};
use uuid::Uuid;
use validator::Validate;

// ---------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/bands",
    tags = ["Bands"],
    summary = "List every band the current user belongs to.",
    security((), ("jwt_token" = [])),
    responses((status = 200, description = "Bands retrieved successfully."))
)]
pub async fn find_all_bands(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, "Processing request to list the current user's bands");

    let bands = state.band_repo.find_all_for_user(user_id).await?;

    info!(%user_id, count = bands.len(), "Bands retrieved successfully");
    Ok(Json(bands))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}",
    tags = ["Bands"],
    summary = "Get a band by ID.",
    description = "The caller must be a member of the band.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Band retrieved successfully."),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn find_band_by_id(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to retrieve band by ID");

    match state.band_repo.find_by_id(id, user_id).await? {
        Some(band) => Ok(Json(band)),
        None => Err(ApiError::NotFound),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/bands",
    tags = ["Bands"],
    summary = "Create a new band.",
    description = "The creator automatically becomes the band's owner.",
    request_body = CreateBandPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Band created successfully.", body = Band),
        (status = 400, description = "Invalid input.")
    )
)]
pub async fn create_band(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CreateBandPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_name = %payload.name, "Processing request to create a new band");

    payload.validate()?;

    let new_band = state.band_repo.create(&payload, user_id).await?;

    info!(%user_id, band_id = %new_band.id, "Band created successfully");

    let mut headers = HeaderMap::new();
    let location = format!("/api/v1/bands/{}", new_band.id);
    if let Ok(header_value) = HeaderValue::from_str(&location) {
        headers.insert(LOCATION, header_value);
    }

    Ok((StatusCode::CREATED, headers, Json(new_band)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/bands/{id}",
    tags = ["Bands"],
    summary = "Update a band's details.",
    description = "Requires the `admin` band role or higher.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = UpdateBandPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Band updated successfully."),
        (status = 403, description = "The caller does not have the required band role."),
        (status = 404, description = "Band not found.")
    )
)]
pub async fn update_band(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateBandPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to update band");

    payload.validate()?;

    state
        .band_repo
        .require_role(id, user_id, BandRole::Admin)
        .await?;

    let band_id = state.band_repo.update(id, &payload).await?;

    info!(%user_id, band_id = %band_id, "Band updated successfully");
    Ok(Json(band_id))
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}",
    tags = ["Bands"],
    summary = "Delete a band.",
    description = "Requires the `owner` band role. This also deletes every setlist owned by the band.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Band deleted successfully"),
        (status = 403, description = "Only the band's owner can delete it.")
    )
)]
pub async fn delete_band(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to delete band");

    state
        .band_repo
        .require_role(id, user_id, BandRole::Owner)
        .await?;

    state.band_repo.delete(id).await?;

    info!(%user_id, band_id = %id, "Band deleted successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/transfer-ownership",
    tags = ["Bands"],
    summary = "Transfer band ownership to another member.",
    description = "The caller must currently be the `owner`. The caller becomes an `admin`.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = TransferOwnershipPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Ownership transferred successfully."),
        (status = 403, description = "The caller is not the band's owner."),
        (status = 404, description = "The target user is not a member of this band.")
    )
)]
pub async fn transfer_ownership(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<TransferOwnershipPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, new_owner_id = %payload.new_owner_id, "Processing request to transfer band ownership");

    payload.validate()?;

    state
        .band_repo
        .require_role(id, user_id, BandRole::Owner)
        .await?;

    if payload.new_owner_id == user_id {
        return Err(ApiError::NotModified);
    }

    state
        .band_member_repo
        .transfer_ownership(id, user_id, payload.new_owner_id)
        .await?;

    info!(%user_id, band_id = %id, new_owner_id = %payload.new_owner_id, "Band ownership transferred successfully");
    Ok(Json("Ownership transferred successfully"))
}

// ---------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/members",
    tags = ["Bands"],
    summary = "List a band's members.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses((status = 200, description = "Members retrieved successfully."))
)]
pub async fn list_band_members(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to list band members");

    // Any member can see the roster.
    state
        .band_repo
        .require_role(id, user_id, BandRole::Member)
        .await?;

    let members = state.band_member_repo.list(id).await?;

    info!(%user_id, band_id = %id, count = members.len(), "Band members retrieved successfully");
    Ok(Json(members))
}

#[utoipa::path(
    patch,
    path = "/api/v1/bands/{id}/members/{user_id}",
    tags = ["Bands"],
    summary = "Change a band member's role.",
    description = "Requires `admin` or higher, and callers may only manage members below their own role. Use the dedicated transfer-ownership endpoint to hand off `owner`.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("user_id" = Uuid, Path, description = "The ID of the member to update")
    ),
    request_body = UpdateBandMemberRolePayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Member role updated successfully."),
        (status = 403, description = "The caller does not outrank the target member."),
        (status = 404, description = "Member not found.")
    )
)]
pub async fn update_band_member_role(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateBandMemberRolePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, %band_id, %target_user_id, new_role = %payload.role, "Processing request to update band member role");

    payload.validate()?;

    if payload.role == BandRole::Owner {
        error!("Ownership cannot be granted via role update; use transfer-ownership instead.");
        return Err(ApiError::Forbidden);
    }

    if target_user_id == user_id {
        error!(%user_id, "A member cannot change their own role.");
        return Err(ApiError::Forbidden);
    }

    let caller_role = state
        .band_repo
        .require_role(band_id, user_id, BandRole::Admin)
        .await?;

    let target_role = state
        .band_repo
        .role_of(band_id, target_user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if target_role >= caller_role {
        error!(%user_id, %target_user_id, "Cannot manage a member with an equal or higher role.");
        return Err(ApiError::Forbidden);
    }

    state
        .band_member_repo
        .update_role(band_id, target_user_id, payload.role)
        .await?;

    info!(%user_id, %band_id, %target_user_id, new_role = %payload.role, "Band member role updated successfully");
    Ok(Json("Member role updated successfully"))
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}/members/{user_id}",
    tags = ["Bands"],
    summary = "Remove a member from a band, or leave it yourself.",
    description = "A member removing themselves is treated as leaving the band. The band's owner must transfer ownership before leaving.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("user_id" = Uuid, Path, description = "The ID of the member to remove")
    ),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Member removed successfully"),
        (status = 403, description = "The caller cannot remove this member."),
        (status = 404, description = "Member not found.")
    )
)]
pub async fn remove_band_member(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, %band_id, %target_user_id, "Processing request to remove band member");

    let caller_role = state
        .band_repo
        .role_of(band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if target_user_id == user_id {
        // Leaving the band. The owner must transfer ownership first so the
        // band is never left without one.
        if caller_role == BandRole::Owner {
            error!(%user_id, %band_id, "Band owner attempted to leave without transferring ownership first.");
            return Err(ApiError::Forbidden);
        }
    } else {
        if !caller_role.satisfies(BandRole::Admin) {
            return Err(ApiError::Forbidden);
        }

        let target_role = state
            .band_repo
            .role_of(band_id, target_user_id)
            .await?
            .ok_or(ApiError::NotFound)?;

        if target_role >= caller_role {
            error!(%user_id, %target_user_id, "Cannot remove a member with an equal or higher role.");
            return Err(ApiError::Forbidden);
        }
    }

    state
        .band_member_repo
        .remove(band_id, target_user_id)
        .await?;

    info!(%user_id, %band_id, %target_user_id, "Band member removed successfully");
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Invites
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/bands/{id}/invites",
    tags = ["Bands"],
    summary = "Create an invite link for a band.",
    description = "Requires `admin` or higher.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = CreateBandInvitePayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 201, description = "Invite created successfully."),
        (status = 403, description = "The caller does not have the required band role.")
    )
)]
pub async fn create_band_invite(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<CreateBandInvitePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to create a band invite");

    payload.validate()?;

    let caller_role = state
        .band_repo
        .require_role(id, user_id, BandRole::Admin)
        .await?;

    // Nobody can mint an invite for a role higher than their own.
    if let Some(role) = payload.role
        && role >= caller_role
    {
        error!(%user_id, "Cannot create an invite granting a role equal to or higher than the caller's own.");
        return Err(ApiError::Forbidden);
    }

    let invite = state.band_invite_repo.create(id, user_id, &payload).await?;

    info!(%user_id, band_id = %id, invite_code = %invite.code, "Band invite created successfully");
    Ok((StatusCode::CREATED, Json(invite)))
}

#[utoipa::path(
    get,
    path = "/api/v1/bands/{id}/invites",
    tags = ["Bands"],
    summary = "List a band's invites.",
    description = "Requires `admin` or higher.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses((status = 200, description = "Invites retrieved successfully."))
)]
pub async fn list_band_invites(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to list band invites");

    state
        .band_repo
        .require_role(id, user_id, BandRole::Admin)
        .await?;

    let invites = state.band_invite_repo.list(id).await?;

    info!(%user_id, band_id = %id, count = invites.len(), "Band invites retrieved successfully");
    Ok(Json(invites))
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}/invites/{invite_id}",
    tags = ["Bands"],
    summary = "Revoke a band invite.",
    description = "Requires `admin` or higher.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("invite_id" = Uuid, Path, description = "The ID of the invite to revoke")
    ),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Invite revoked successfully"),
        (status = 404, description = "Invite not found.")
    )
)]
pub async fn revoke_band_invite(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, invite_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, %band_id, %invite_id, "Processing request to revoke band invite");

    state
        .band_repo
        .require_role(band_id, user_id, BandRole::Admin)
        .await?;

    state.band_invite_repo.revoke(invite_id, band_id).await?;

    info!(%user_id, %band_id, %invite_id, "Band invite revoked successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/invites/{code}/accept",
    tags = ["Bands"],
    summary = "Accept a band invite and join the band.",
    params(("code" = String, Path, description = "The invite code")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Joined the band successfully."),
        (status = 404, description = "The invite does not exist, is expired, revoked, or exhausted."),
        (status = 409, description = "The caller is already a member of this band.")
    )
)]
pub async fn accept_band_invite(
    State(state): State<AppState>,
    access: AccessControl,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, invite_code = %code, "Processing request to accept a band invite");

    let invite = state
        .band_invite_repo
        .find_by_code(&code)
        .await?
        .ok_or(ApiError::NotFound)?;

    let is_expired = invite
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now().naive_utc());
    let is_exhausted = invite
        .max_uses
        .is_some_and(|max_uses| invite.uses_count >= max_uses);

    if invite.revoked_at.is_some() || is_expired || is_exhausted {
        error!(invite_code = %code, "Attempted to use an invalid or exhausted invite.");
        return Err(ApiError::NotFound);
    }

    state
        .band_member_repo
        .add_member(invite.band_id, user_id, invite.role)
        .await?;

    state.band_invite_repo.increment_uses(invite.id).await?;

    let band = state
        .band_repo
        .find_by_id(invite.band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    info!(%user_id, band_id = %invite.band_id, "User joined band via invite successfully");
    Ok(Json(band))
}
