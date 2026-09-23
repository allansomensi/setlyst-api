use crate::{
    controllers::pin::{mark_one, mark_pinned},
    database::{AppState, repositories::band_invite_repository::InviteLimits},
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        band::{
            Band, BandRole, CreateBandInvitePayload, CreateBandPayload, TransferOwnershipPayload,
            UpdateBandMemberRolePayload, UpdateBandMemberTitlePayload, UpdateBandPayload,
            UpdateBandRolePermissionsPayload,
        },
        notification::Notification,
        quota::QuotaResource,
    },
    services::{
        entitlements::{Feature, ensure_feature},
        notifier::notify,
    },
    utils::share_token::token_fingerprint,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION},
    response::IntoResponse,
};
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

    let mut bands = state.band_repo.find_all_for_user(user_id).await?;
    mark_pinned(&state, user_id, &mut bands).await?;

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
        Some(mut band) => {
            mark_one(&state, user_id, &mut band).await?;
            Ok(Json(band))
        }
        None => Err(ApiError::NotFound),
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/bands",
    tags = ["Bands"],
    summary = "Create a new band.",
    description = "The creator automatically becomes the band's owner. The band's repertoire is created with it. Requires the plan feature `create_bands`.",
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
    ensure_feature(&state, user_id, Feature::CreateBands).await?;

    state
        .quota_repo
        .ensure_user(user_id, QuotaResource::BandsOwned, 1)
        .await?;
    state
        .quota_repo
        .ensure_user(user_id, QuotaResource::BandMemberships, 1)
        .await?;

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
    description = "Requires the `admin` band role or higher. A new `logo_url` must be a public `https` image link (`INVALID_IMAGE_URL`) and is reviewed by the automatic moderation in the background. `suggestion_auto_accept_votes` (1..100, `null` = off) sets how many up-votes accept a song suggestion automatically.",
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

    let mut payload = payload;
    let new_logo = match &payload.logo_url {
        Some(Some(url)) if !url.trim().is_empty() => {
            let url = crate::validations::image_url::validate_image_url(url)?;
            payload.logo_url = Some(Some(url.clone()));
            Some(url)
        }
        _ => None,
    };

    let band_id = state.band_repo.update(id, &payload, user_id).await?;

    if let Some(url) = new_logo {
        crate::moderation::spawn_band_logo_review(&state, band_id, &url);
    }

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
    path = "/api/v1/bands/{id}/favorite",
    tags = ["Bands"],
    summary = "Favorite a band.",
    description = "Purely personal to the caller — never affects anyone else's view of the band, and grants no permission. Idempotent. The caller must be a member of the band.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Favorited successfully."),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn favorite_band(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to favorite band");

    state
        .band_repo
        .require_role(id, user_id, BandRole::Member)
        .await?;
    state.band_repo.add_favorite(id, user_id).await?;

    info!(%user_id, band_id = %id, "Band favorited successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/bands/{id}/favorite",
    tags = ["Bands"],
    summary = "Un-favorite a band.",
    description = "Idempotent — un-favoriting a band that isn't favorited is not an error.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Un-favorited successfully.")
    )
)]
pub async fn unfavorite_band(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to unfavorite band");

    state.band_repo.remove_favorite(id, user_id).await?;

    info!(%user_id, band_id = %id, "Band unfavorited successfully");
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
    description = "Requires `admin` or higher, and callers may only manage members below their own role. Only the owner may grant a role equal to or above their own (i.e. `admin`); an admin can promote up to `moderator`. Use the dedicated transfer-ownership endpoint to hand off `owner`.",
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

    // Nobody but the owner can hand out a role as high as their own.
    if payload.role >= caller_role && caller_role != BandRole::Owner {
        error!(%user_id, %target_user_id, "Cannot grant a role equal to or higher than the caller's own.");
        return Err(ApiError::Forbidden);
    }

    state
        .band_member_repo
        .update_role(band_id, target_user_id, payload.role)
        .await?;

    if let Some(band) = state.band_repo.find_by_id(band_id, user_id).await? {
        notify(
            &state,
            Notification::band_role_changed(
                target_user_id,
                band_id,
                &band.name,
                target_role,
                payload.role,
                user_id,
            ),
        )
        .await;
    }

    info!(%user_id, %band_id, %target_user_id, new_role = %payload.role, "Band member role updated successfully");
    Ok(Json("Member role updated successfully"))
}

#[utoipa::path(
    patch,
    path = "/api/v1/bands/{id}/members/{user_id}/title",
    tags = ["Bands"],
    summary = "Set or clear a band member's free-text title/function.",
    description = "Purely cosmetic (e.g. \"Guitarrista\", \"Baixista\") — never affects permissions. A member may set their own title; an `admin` or higher may set anyone's.",
    params(
        ("id" = Uuid, Path, description = "The ID of the band"),
        ("user_id" = Uuid, Path, description = "The ID of the member to update")
    ),
    request_body = UpdateBandMemberTitlePayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Member title updated successfully."),
        (status = 403, description = "The caller may only set their own title, or must be an admin."),
        (status = 404, description = "Member not found.")
    )
)]
pub async fn update_band_member_title(
    State(state): State<AppState>,
    access: AccessControl,
    Path((band_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateBandMemberTitlePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, %band_id, %target_user_id, "Processing request to update band member title");

    payload.validate()?;

    if target_user_id != user_id {
        state
            .band_repo
            .require_role(band_id, user_id, BandRole::Admin)
            .await?;
    } else {
        // Still ensure the caller is actually a member of this band.
        state
            .band_repo
            .require_role(band_id, user_id, BandRole::Member)
            .await?;
    }

    state
        .band_member_repo
        .update_title(band_id, target_user_id, payload.title.as_deref())
        .await?;

    info!(%user_id, %band_id, %target_user_id, "Band member title updated successfully");
    Ok(Json("Member title updated successfully"))
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

    // Only notify when someone else removed the member — voluntarily
    // leaving a band is not a notification-worthy event for the leaver.
    if target_user_id != user_id
        && let Some(band) = state.band_repo.find_by_id(band_id, user_id).await?
    {
        notify(
            &state,
            Notification::band_member_removed(target_user_id, band_id, &band.name, user_id),
        )
        .await;
    }

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
    description = "Requires `admin` or higher. Codes are 16 characters long; invites expire after 7 days unless `expires_in_hours` says otherwise (up to one year).",
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

    info!(%user_id, band_id = %id, invite = %token_fingerprint(&invite.code), "Band invite created successfully");
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
    get,
    path = "/api/v1/bands/{id}/permissions",
    tags = ["Bands"],
    summary = "Get the band's member/moderator permission matrix.",
    description = "Requires the `admin` band role or higher. `admin` and `owner` are never restricted and are not included in the response.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    security((), ("jwt_token" = [])),
    responses(
        (status = 200, description = "Permissions retrieved successfully."),
        (status = 403, description = "The caller does not have the required band role."),
        (status = 404, description = "Band not found.")
    )
)]
pub async fn get_band_role_permissions(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to fetch band role permissions");

    state
        .band_repo
        .require_role(id, user_id, BandRole::Admin)
        .await?;

    let permissions = state.band_repo.get_role_permissions(id).await?;

    info!(%user_id, band_id = %id, "Band role permissions retrieved successfully");
    Ok(Json(permissions))
}

#[utoipa::path(
    put,
    path = "/api/v1/bands/{id}/permissions",
    tags = ["Bands"],
    summary = "Update the band's member/moderator permission matrix.",
    description = "Requires the `admin` band role or higher. Only `member` and `moderator` rows may be set — `admin`/`owner` always have every permission.",
    params(("id" = Uuid, Path, description = "The ID of the band")),
    request_body = UpdateBandRolePermissionsPayload,
    security((), ("jwt_token" = [])),
    responses(
        (status = 204, description = "Permissions updated successfully."),
        (status = 400, description = "Invalid payload (e.g. tried to set an admin/owner row)."),
        (status = 403, description = "The caller does not have the required band role."),
        (status = 404, description = "Band not found.")
    )
)]
pub async fn update_band_role_permissions(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateBandRolePermissionsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, band_id = %id, "Processing request to update band role permissions");

    payload.validate()?;

    state
        .band_repo
        .require_role(id, user_id, BandRole::Admin)
        .await?;

    state
        .band_repo
        .update_role_permissions(id, &payload.permissions)
        .await?;

    info!(%user_id, band_id = %id, "Band role permissions updated successfully");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/invites/{code}/accept",
    tags = ["Bands"],
    summary = "Accept a band invite and join the band.",
    description = "Rate limited per client IP (one every 2 seconds, bursts of 10). The band's member limit and the caller's band limit are checked atomically with the join (`QUOTA_EXCEEDED`).",
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
    debug!(%user_id, invite = %token_fingerprint(&code), "Processing request to accept a band invite");

    if code.len() > 32 {
        return Err(ApiError::rule(
            StatusCode::NOT_FOUND,
            crate::errors::api_error::codes::INVITE_INVALID,
            "This invite link is invalid, expired or has already been used.",
        ));
    }

    // Peek at the invite only to know which band's limits apply; the
    // actual validation, the limit checks and the redemption happen
    // atomically in `redeem`.
    let mut limits = InviteLimits {
        band_members: None,
        band_memberships: state
            .quota_repo
            .effective_limits(user_id)
            .await?
            .map(|l| l.band_memberships),
    };
    if let Some(invite) = state
        .band_invite_repo
        .find_by_code(&code.trim().to_uppercase())
        .await?
    {
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM band_members WHERE band_id = $1 AND role = 'owner' LIMIT 1",
        )
        .bind(invite.band_id)
        .fetch_optional(&state.db)
        .await?;
        limits.band_members = match owner {
            Some(owner) => state
                .quota_repo
                .effective_limits(owner)
                .await?
                .map(|l| l.band_members),
            None => Some(state.quota_repo.get_defaults().await?.band_members),
        };
    }

    let (band_id, _role) = state
        .band_invite_repo
        .redeem(&code, user_id, limits)
        .await?;

    let band = state
        .band_repo
        .find_by_id(band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    info!(%user_id, %band_id, "User joined band via invite successfully");
    Ok(Json(band))
}
