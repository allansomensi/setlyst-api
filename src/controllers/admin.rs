//! Staff console endpoints (`/admin/...`).
//!
//! Reads and public-link moderation are open to every staff member
//! (moderators and admins); reads of private content are recorded in the
//! audit log. The audit log itself (IP addresses, billing metadata) and
//! anything that changes someone else's content, memberships or the
//! platform limits are admin-only.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse,
        admin::{
            AdminBandDetail, AdminBandSummary, AdminListQuery, AdminSetlistDetail,
            AdminSetlistSummary, AdminSongDetail, AdminSongSummary, AdminTransferOwnershipPayload,
            AdminUpdateBandMemberRolePayload, RevokeSharePayload, SharedLink,
        },
        audit::{AuditLogEntry, AuditLogQuery, actions},
        auth::access::{AccessControl, ClientIp},
        band::{AdminAddBandMemberPayload, BandRole, UpdateBandPayload},
        billing::{StaffRefundPayload, WithdrawResponse},
        notification::Notification,
        quota::QuotaLimits,
        setlist::UpdateSetlistPayload,
        song::{SongWithArtist, UpdateSongPayload},
    },
    services::payments,
    validations::tag::normalize_tags,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;
use validator::Validate;

use crate::services::notifier::notify;

// ---------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/bands",
    tags = ["Admin"],
    summary = "List every band on the platform.",
    params(AdminListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Bands.", body = PaginatedResponse<AdminBandSummary>))
)]
pub async fn list_bands(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AdminListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = query.page();
    let (bands, total) = state.admin_repo.list_bands(&query).await?;
    Ok(Json(PaginatedResponse::new(bands, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/bands/{id}",
    tags = ["Admin"],
    summary = "A band with its full roster.",
    params(("id" = Uuid, Path, description = "Band ID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Band detail.", body = AdminBandDetail))
)]
pub async fn get_band(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let members = state.band_member_repo.list(id).await?;
    Ok(Json(AdminBandDetail { band, members }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/bands/{id}",
    tags = ["Admin"],
    summary = "Edit any band. Admin only.",
    params(("id" = Uuid, Path, description = "Band ID")),
    request_body = UpdateBandPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated band.", body = AdminBandSummary))
)]
pub async fn update_band(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateBandPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;

    let before = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    state
        .band_repo
        .update(id, &payload, access.user_id())
        .await?;

    AuditEvent::by(&access, actions::BAND_UPDATED)
        .target("band", id, &before.name)
        .meta(json!({ "name": payload.name, "previous_name": before.name }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    state
        .admin_repo
        .find_band(id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/bands/{id}",
    tags = ["Admin"],
    summary = "Delete any band, with all its setlists, songs and gigs. Admin only.",
    params(("id" = Uuid, Path, description = "Band ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Band deleted."))
)]
pub async fn delete_band(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state.band_repo.delete(id).await?;

    AuditEvent::by(&access, actions::BAND_DELETED)
        .target("band", id, &band.name)
        .meta(json!({
            "owner": band.owner_username,
            "members": band.member_count,
            "setlists": band.setlist_count,
            "songs": band.song_count,
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/bands/{id}/members",
    tags = ["Admin"],
    summary = "Add any user to a band. Admin only.",
    description = "Bypasses invites. The user is notified. `owner` can't be granted here — use transfer-ownership.",
    params(("id" = Uuid, Path, description = "Band ID")),
    request_body = AdminAddBandMemberPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 201, description = "Member added."),
        (status = 409, description = "Already a member.")
    )
)]
pub async fn add_band_member(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<AdminAddBandMemberPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;

    let role = payload.role.unwrap_or(BandRole::Member);
    if role == BandRole::Owner {
        return Err(ApiError::BadRequest(
            "Ownership can't be granted directly; transfer it instead.".to_string(),
        ));
    }

    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let user = state
        .user_repo
        .find_by_id(payload.user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state
        .band_member_repo
        .add_member(id, payload.user_id, role)
        .await?;

    notify(
        &state,
        Notification::band_member_added(payload.user_id, id, &band.name, role, access.user_id()),
    )
    .await;

    AuditEvent::by(&access, actions::BAND_MEMBER_ADDED)
        .target("band", id, &band.name)
        .meta(json!({ "user_id": user.id, "username": user.username, "role": role }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/bands/{id}/members/{user_id}",
    tags = ["Admin"],
    summary = "Change any member's band role. Admin only.",
    params(
        ("id" = Uuid, Path, description = "Band ID"),
        ("user_id" = Uuid, Path, description = "Member's user ID")
    ),
    request_body = AdminUpdateBandMemberRolePayload,
    security(("jwt_token" = [])),
    responses((status = 204, description = "Role changed."))
)]
pub async fn update_band_member_role(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<AdminUpdateBandMemberRolePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;

    if payload.role == BandRole::Owner {
        return Err(ApiError::BadRequest(
            "Ownership can't be granted directly; transfer it instead.".to_string(),
        ));
    }

    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let current = state
        .band_repo
        .role_of(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if current == BandRole::Owner {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::INSUFFICIENT_ROLE,
            "The owner's role can't be changed; transfer ownership first.",
        ));
    }

    state
        .band_member_repo
        .update_role(id, user_id, payload.role)
        .await?;

    if current != payload.role {
        notify(
            &state,
            Notification::band_role_changed(
                user_id,
                id,
                &band.name,
                current,
                payload.role,
                access.user_id(),
            ),
        )
        .await;
    }

    AuditEvent::by(&access, actions::BAND_MEMBER_ROLE_CHANGED)
        .target("band", id, &band.name)
        .meta(json!({ "user_id": user_id, "from": current, "to": payload.role }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/bands/{id}/members/{user_id}",
    tags = ["Admin"],
    summary = "Remove any member from a band. Admin only.",
    description = "The owner can't be removed — transfer ownership first.",
    params(
        ("id" = Uuid, Path, description = "Band ID"),
        ("user_id" = Uuid, Path, description = "Member's user ID")
    ),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Member removed."))
)]
pub async fn remove_band_member(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;

    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let role = state
        .band_repo
        .role_of(id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if role == BandRole::Owner {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::INSUFFICIENT_ROLE,
            "The owner can't be removed; transfer ownership first.",
        ));
    }

    state.band_member_repo.remove(id, user_id).await?;

    notify(
        &state,
        Notification::band_member_removed(user_id, id, &band.name, access.user_id()),
    )
    .await;

    AuditEvent::by(&access, actions::BAND_MEMBER_REMOVED)
        .target("band", id, &band.name)
        .meta(json!({ "user_id": user_id, "role": role }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/bands/{id}/transfer-ownership",
    tags = ["Admin"],
    summary = "Hand a band to another member. Admin only.",
    description = "The new owner must already be a member; the previous owner becomes an admin of the band.",
    params(("id" = Uuid, Path, description = "Band ID")),
    request_body = AdminTransferOwnershipPayload,
    security(("jwt_token" = [])),
    responses((status = 204, description = "Ownership transferred."))
)]
pub async fn transfer_band_ownership(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<AdminTransferOwnershipPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;

    let band = state
        .admin_repo
        .find_band(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    match band.owner_id {
        Some(owner) if owner == payload.new_owner_id => return Err(ApiError::NotModified),
        Some(owner) => {
            state
                .band_member_repo
                .transfer_ownership(id, owner, payload.new_owner_id)
                .await?
        }
        None => {
            state
                .band_member_repo
                .update_role(id, payload.new_owner_id, BandRole::Owner)
                .await?
        }
    }

    AuditEvent::by(&access, actions::BAND_OWNERSHIP_TRANSFERRED)
        .target("band", id, &band.name)
        .meta(json!({ "from": band.owner_id, "to": payload.new_owner_id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Songs
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/songs",
    tags = ["Admin"],
    summary = "List songs across the platform.",
    params(AdminListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Songs.", body = PaginatedResponse<AdminSongSummary>))
)]
pub async fn list_songs(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AdminListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = query.page();
    let (songs, total) = state.admin_repo.list_songs(&query).await?;
    Ok(Json(PaginatedResponse::new(songs, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/songs/{id}",
    tags = ["Admin"],
    summary = "Any song, with lyrics.",
    params(("id" = Uuid, Path, description = "Song ID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Song.", body = AdminSongDetail))
)]
pub async fn get_song(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (song, summary) = tokio::try_join!(
        state.song_repo.find_with_artist_name(id),
        state.admin_repo.find_song(id),
    )?;
    match (song, summary) {
        (Some(song), Some(summary)) => {
            // Staff reading private lyrics is recorded (LGPD
            // accountability).
            AuditEvent::by(&access, actions::STAFF_CONTENT_VIEWED)
                .target("song", id, &song.title)
                .ip(&ip.0)
                .spawn(state.audit_repo.clone());
            Ok(Json(AdminSongDetail { song, summary }))
        }
        _ => Err(ApiError::NotFound),
    }
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/songs/{id}",
    tags = ["Admin"],
    summary = "Edit any song. Admin only.",
    description = "Same fields as the owner's edit, except the artist.",
    params(("id" = Uuid, Path, description = "Song ID")),
    request_body = UpdateSongPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated song.", body = SongWithArtist))
)]
pub async fn update_song(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSongPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;

    if payload.artist_id.is_some() {
        return Err(ApiError::BadRequest(
            "The artist of someone else's song can't be changed.".to_string(),
        ));
    }
    let tags = payload.tags.as_deref().map(normalize_tags).transpose()?;

    let before = state
        .song_repo
        .find_with_artist_name(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state
        .song_repo
        .update(id, &payload, tags.as_deref(), access.user_id())
        .await?;

    AuditEvent::by(&access, actions::SONG_UPDATED)
        .target("song", id, &before.title)
        .meta(json!({ "owner_id": before.user_id, "band_id": before.band_id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    state
        .song_repo
        .find_with_artist_name(id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/songs/{id}",
    tags = ["Admin"],
    summary = "Delete any song. Admin only.",
    params(("id" = Uuid, Path, description = "Song ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Song deleted."))
)]
pub async fn delete_song(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let song = state
        .song_repo
        .find_with_artist_name(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state.song_repo.delete(id).await?;

    AuditEvent::by(&access, actions::SONG_DELETED)
        .target("song", id, &song.title)
        .meta(json!({ "artist": song.artist_name, "owner_id": song.user_id, "band_id": song.band_id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Setlists
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/setlists",
    tags = ["Admin"],
    summary = "List setlists across the platform.",
    params(AdminListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Setlists.", body = PaginatedResponse<AdminSetlistSummary>))
)]
pub async fn list_setlists(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AdminListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = query.page();
    let (setlists, total) = state.admin_repo.list_setlists(&query).await?;
    Ok(Json(PaginatedResponse::new(
        setlists, total, page, per_page,
    )))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/setlists/{id}",
    tags = ["Admin"],
    summary = "Any setlist, with its running order.",
    params(("id" = Uuid, Path, description = "Setlist ID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Setlist.", body = AdminSetlistDetail))
)]
pub async fn get_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let setlist = state
        .admin_repo
        .find_setlist(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let items = state.setlist_repo.get_items(id).await?;
    AuditEvent::by(&access, actions::STAFF_CONTENT_VIEWED)
        .target("setlist", id, &setlist.title)
        .ip(&ip.0)
        .spawn(state.audit_repo.clone());
    Ok(Json(AdminSetlistDetail { setlist, items }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/setlists/{id}",
    tags = ["Admin"],
    summary = "Edit any setlist's title/description. Admin only.",
    params(("id" = Uuid, Path, description = "Setlist ID")),
    request_body = UpdateSetlistPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated setlist.", body = AdminSetlistSummary))
)]
pub async fn update_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSetlistPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;

    let before = state
        .admin_repo
        .find_setlist(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    if let Some(title) = &payload.title {
        state
            .setlist_repo
            .is_unique(title.trim(), before.user_id, before.band_id, Some(id))
            .await?;
    }

    state
        .setlist_repo
        .update(id, &payload, access.user_id())
        .await?;

    AuditEvent::by(&access, actions::SETLIST_UPDATED)
        .target("setlist", id, &before.title)
        .meta(json!({ "title": payload.title, "owner_id": before.user_id }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    state
        .admin_repo
        .find_setlist(id)
        .await?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/setlists/{id}",
    tags = ["Admin"],
    summary = "Delete any setlist. Admin only.",
    params(("id" = Uuid, Path, description = "Setlist ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Setlist deleted."))
)]
pub async fn delete_setlist(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let setlist = state
        .admin_repo
        .find_setlist(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state.setlist_repo.delete(id).await?;

    AuditEvent::by(&access, actions::SETLIST_DELETED)
        .target("setlist", id, &setlist.title)
        .meta(json!({ "owner": setlist.owner_username, "band": setlist.band_name }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Public links
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/shared-links",
    tags = ["Admin"],
    summary = "Every active or locked public link.",
    params(AdminListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Links.", body = PaginatedResponse<SharedLink>))
)]
pub async fn list_shared_links(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AdminListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = query.page();
    let (links, total) = state.admin_repo.list_shared_links(&query).await?;
    Ok(Json(PaginatedResponse::new(links, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/setlists/{id}/share/revoke",
    tags = ["Admin"],
    summary = "Take a setlist's public link down immediately.",
    description = "The link stops working at once and the owner can't re-share until staff unlocks it. The owner is notified with the reason.",
    params(("id" = Uuid, Path, description = "Setlist ID")),
    request_body = RevokeSharePayload,
    security(("jwt_token" = [])),
    responses((status = 204, description = "Link revoked and locked."))
)]
pub async fn revoke_setlist_share(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<RevokeSharePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let reason = payload
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());

    let setlist = state
        .setlist_repo
        .find_any(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state
        .setlist_repo
        .lock_sharing(id, access.user_id(), reason)
        .await?;

    notify(
        &state,
        Notification::share_link_revoked(
            setlist.user_id,
            "setlist",
            id,
            &setlist.title,
            reason,
            access.user_id(),
        ),
    )
    .await;

    AuditEvent::by(&access, actions::SHARE_REVOKED)
        .target("setlist", id, &setlist.title)
        .meta(json!({ "reason": reason, "owner_id": setlist.user_id, "was_shared": setlist.share_token.is_some() }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/setlists/{id}/share/unlock",
    tags = ["Admin"],
    summary = "Allow a setlist to be shared again.",
    description = "Doesn't re-enable the old link — the owner must share it again, which generates a new one.",
    params(("id" = Uuid, Path, description = "Setlist ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Unlocked."))
)]
pub async fn unlock_setlist_share(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let setlist = state
        .setlist_repo
        .find_any(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    state.setlist_repo.unlock_sharing(id).await?;

    AuditEvent::by(&access, actions::SHARE_UNLOCKED)
        .target("setlist", id, &setlist.title)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/gigs/{id}/share/revoke",
    tags = ["Admin"],
    summary = "Take a gig's public link down immediately.",
    params(("id" = Uuid, Path, description = "Gig ID")),
    request_body = RevokeSharePayload,
    security(("jwt_token" = [])),
    responses((status = 204, description = "Link revoked and locked."))
)]
pub async fn revoke_gig_share(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<RevokeSharePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let reason = payload
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());

    let gig = state
        .gig_repo
        .find_any(id)
        .await?
        .ok_or(ApiError::NotFound)?;

    state
        .gig_repo
        .lock_sharing(id, access.user_id(), reason)
        .await?;

    notify(
        &state,
        Notification::share_link_revoked(
            gig.user_id,
            "gig",
            id,
            &gig.venue,
            reason,
            access.user_id(),
        ),
    )
    .await;

    AuditEvent::by(&access, actions::SHARE_REVOKED)
        .target("gig", id, &gig.venue)
        .meta(json!({ "reason": reason, "owner_id": gig.user_id, "was_shared": gig.share_token.is_some() }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/gigs/{id}/share/unlock",
    tags = ["Admin"],
    summary = "Allow a gig to be shared again.",
    params(("id" = Uuid, Path, description = "Gig ID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Unlocked."))
)]
pub async fn unlock_gig_share(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let gig = state
        .gig_repo
        .find_any(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    state.gig_repo.unlock_sharing(id).await?;

    AuditEvent::by(&access, actions::SHARE_UNLOCKED)
        .target("gig", id, &gig.venue)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------
// Audit log & platform settings
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/audit-logs",
    tags = ["Admin"],
    summary = "The audit log, newest first. Admin only.",
    params(AuditLogQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Entries.", body = PaginatedResponse<AuditLogEntry>))
)]
pub async fn list_audit_logs(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<AuditLogQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // IP addresses and billing metadata of every account: not for
    // moderators.
    access.require_admin()?;
    let (page, per_page) = crate::models::resolve_page(query.page, query.per_page, 50);
    let (entries, total) = state.audit_repo.list(&query, page, per_page).await?;
    Ok(Json(PaginatedResponse::new(entries, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/settings/quotas",
    tags = ["Admin"],
    summary = "Platform-wide default limits.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Defaults.", body = QuotaLimits))
)]
pub async fn get_quota_defaults(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.quota_repo.get_defaults().await?))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/settings/quotas",
    tags = ["Admin"],
    summary = "Replace the platform-wide default limits. Admin only.",
    request_body = QuotaLimits,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Saved defaults.", body = QuotaLimits))
)]
pub async fn update_quota_defaults(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<QuotaLimits>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;

    let previous = state.quota_repo.get_defaults().await?;
    state
        .quota_repo
        .set_defaults(&payload, access.user_id())
        .await?;

    AuditEvent::by(&access, actions::QUOTA_DEFAULTS_UPDATED)
        .meta(json!({ "from": previous, "to": payload }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;

    Ok(Json(payload))
}

// ---------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/v1/admin/users/{id}/subscription/refund",
    tags = ["Billing admin"],
    summary = "Refund and cancel a user's paid subscription now (admin).",
    description = "Cancels the account's paid subscription at the payment provider immediately and refunds it: every charge inside the 7-day withdrawal window when it is still open, otherwise the latest charge in full. The account loses the plan at once, gets a `withdrawal_confirmed` e-mail and the action is audited as `billing.subscription_refunded` (with `reason`). Admins only. Errors: `NO_PAID_SUBSCRIPTION` (409), `PAYMENTS_UNAVAILABLE`, `PAYMENT_PROVIDER_ERROR` (502, nothing changed: repeat the request).",
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = StaffRefundPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Refunded and canceled.", body = WithdrawResponse),
        (status = 403, description = "Not an admin."),
        (status = 404, description = "No such user."),
        (status = 409, description = "No paid subscription."),
        (status = 502, description = "The payment provider failed."),
    )
)]
pub async fn refund_user_subscription(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<StaffRefundPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let reason = payload.reason.trim();
    if reason.is_empty() {
        return Err(ApiError::BadRequest("A reason is required.".into()));
    }
    // Never one's own charges (outside the buyer's own withdrawal window
    // that would be a self-granted refund); another admin does it.
    if id == access.user_id() {
        return Err(ApiError::cannot_target_self(
            "You can't refund your own subscription here.",
        ));
    }
    if state.user_repo.find_by_id(id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let refunded = payments::refund_and_cancel(&state, id, access.user_id(), reason, ip.0).await?;
    Ok(Json(refunded))
}
