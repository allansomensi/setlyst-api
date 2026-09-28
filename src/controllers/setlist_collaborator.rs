//! Collaborators of personal setlists: sharing a setlist with other
//! accounts without a band (see `0017_setlist_collaboration.sql`).

use crate::{
    controllers::pin::mark_pinned,
    database::AppState,
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse, PaginationMeta, PaginationQuery,
        auth::access::AccessControl,
        notification::Notification,
        setlist::Setlist,
        setlist_collaborator::{
            CandidateStatus, CollaboratorCandidate, CollaboratorLookupQuery, CollaboratorRole,
            InviteCollaboratorPayload, SetlistCollaborators, SetlistInvitation,
            UpdateCollaboratorPayload,
        },
        user::{Status, active_ban},
    },
    services::{account::require_verified_email, notifier::notify},
    utils::rate_limit::presets,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::{debug, info};
use uuid::Uuid;
use validator::Validate;

fn user_not_found() -> ApiError {
    ApiError::rule(
        StatusCode::NOT_FOUND,
        codes::USER_NOT_FOUND,
        "No account with this username.",
    )
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/shared",
    tags = ["Setlists"],
    summary = "List the setlists shared with the caller.",
    description = "Personal setlists of other accounts the caller collaborates on (accepted invites), favorites first, then by the latest change. Each carries the caller's `collaborator_role`.",
    params(PaginationQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Setlists shared with the caller.", body = PaginatedResponse<Setlist>))
)]
pub async fn find_shared_setlists(
    State(state): State<AppState>,
    access: AccessControl,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (current_page, per_page) = pagination.resolve();
    debug!(%user_id, current_page, per_page, "Processing request to list shared setlists");

    let (mut setlists, total_items) = state
        .setlist_repo
        .find_shared(user_id, current_page, per_page)
        .await?;
    mark_pinned(&state, user_id, &mut setlists).await?;
    // Only the owner ever sees a personal setlist's public link.
    for setlist in &mut setlists {
        setlist.share_token = None;
    }
    let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

    Ok(Json(PaginatedResponse {
        data: setlists,
        meta: PaginationMeta {
            total_items,
            current_page,
            per_page,
            total_pages,
        },
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/invitations",
    tags = ["Setlists"],
    summary = "List the caller's pending setlist invites.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Pending invites, newest first.", body = [SetlistInvitation]))
)]
pub async fn find_setlist_invitations(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let invitations = state.setlist_collaborator_repo.invitations(user_id).await?;
    Ok(Json(invitations))
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/invitation/accept",
    tags = ["Setlists"],
    summary = "Accept an invite to collaborate on a setlist.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Invite accepted: the setlist is now shared with the caller."),
        (status = 404, description = "No pending invite to this setlist.")
    )
)]
pub async fn accept_setlist_invitation(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .setlist_collaborator_repo
        .accept(setlist_id, user_id)
        .await?;
    info!(%user_id, %setlist_id, "Setlist invite accepted");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/invitation",
    tags = ["Setlists"],
    summary = "Decline an invite to collaborate on a setlist.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Invite declined."),
        (status = 404, description = "No pending invite to this setlist.")
    )
)]
pub async fn decline_setlist_invitation(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    state
        .setlist_collaborator_repo
        .decline(setlist_id, user_id)
        .await?;
    info!(%user_id, %setlist_id, "Setlist invite declined");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/collaborators",
    tags = ["Setlists"],
    summary = "List who a setlist is shared with.",
    description = "The owner and every collaborator (pending invites included). Anyone who can see the setlist. Band setlists have no collaborators (`COLLABORATION_UNAVAILABLE`).",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Owner and collaborators.", body = SetlistCollaborators),
        (status = 404, description = "Setlist not found."),
        (status = 409, description = "A band setlist.")
    )
)]
pub async fn list_setlist_collaborators(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if setlist.band_id.is_some() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::COLLABORATION_UNAVAILABLE,
            "Only personal setlists can have collaborators.",
        ));
    }
    let collaborators = state.setlist_collaborator_repo.list(setlist_id).await?;
    Ok(Json(collaborators))
}

#[utoipa::path(
    get,
    path = "/api/v1/setlists/{id}/collaborators/lookup",
    tags = ["Setlists"],
    summary = "Look up an account before inviting it to a setlist.",
    description = "Confirms that a username belongs to an account that can be invited, before sending the invite: its username and avatar, and where it already stands in the setlist (`status`: `available`, `invited`, `collaborator`, `owner` or `self`). Only for who may invite (the owner and managers; `403` otherwise). An unknown, inactive or banned account is `USER_NOT_FOUND`, as in the invite itself. At most 120 lookups per 10 minutes (`TOO_MANY_ATTEMPTS`).",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        CollaboratorLookupQuery
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The account found.", body = CollaboratorCandidate),
        (status = 403, description = "The caller can't invite people to this setlist."),
        (status = 404, description = "Setlist or user not found."),
        (status = 409, description = "A band setlist.")
    )
)]
pub async fn lookup_setlist_collaborator(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Query(query): Query<CollaboratorLookupQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    // The setlist first, as for the invite: a stranger must not learn
    // which usernames exist through this endpoint.
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if setlist.band_id.is_some() {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::COLLABORATION_UNAVAILABLE,
            "Only personal setlists can have collaborators.",
        ));
    }
    let standing = state
        .setlist_collaborator_repo
        .standing(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !standing.can_manage(CollaboratorRole::Viewer) {
        return Err(ApiError::Forbidden);
    }
    presets::limit(&presets::SETLIST_COLLABORATOR_LOOKUPS, user_id)?;

    let username = query.username.trim().trim_start_matches('@');
    if username.is_empty() || username.chars().count() > 50 {
        return Err(user_not_found());
    }
    let candidate = state
        .user_repo
        .find_by_username(username)
        .await?
        .ok_or_else(user_not_found)?;
    if candidate.status != Status::Active
        || active_ban(
            candidate.banned_at,
            candidate.banned_until,
            chrono::Utc::now().naive_utc(),
        )
        .is_some()
    {
        return Err(user_not_found());
    }

    let status = if candidate.id == user_id {
        CandidateStatus::Myself
    } else {
        state
            .setlist_collaborator_repo
            .candidate_status(setlist_id, candidate.id)
            .await?
    };

    Ok(Json(CollaboratorCandidate {
        user_id: candidate.id,
        username: candidate.username,
        avatar_url: candidate.avatar_url,
        status,
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/setlists/{id}/collaborators",
    tags = ["Setlists"],
    summary = "Invite someone to collaborate on a setlist.",
    description = "Invites an account by username, as `viewer`, `editor` (default) or `manager`. The owner invites with any role; a `manager` only viewers and editors. The invite grants access once accepted (`POST /setlists/{id}/invitation/accept`), and the invitee is notified. Personal setlists only (`COLLABORATION_UNAVAILABLE`); at most 20 collaborators including pending invites (`QUOTA_EXCEEDED`, `meta.resource` = `setlist_collaborators`); someone already invited is `ALREADY_MEMBER`; an unknown username is `USER_NOT_FOUND`. Requires a verified e-mail.",
    params(("id" = Uuid, Path, description = "The ID of the setlist")),
    request_body = InviteCollaboratorPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Invite sent."),
        (status = 403, description = "The caller can't invite with this role."),
        (status = 404, description = "Setlist or user not found."),
        (status = 409, description = "Already invited, or a band setlist.")
    )
)]
pub async fn invite_setlist_collaborator(
    State(state): State<AppState>,
    access: AccessControl,
    Path(setlist_id): Path<Uuid>,
    Json(payload): Json<InviteCollaboratorPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    debug!(%user_id, %setlist_id, "Processing request to invite a setlist collaborator");
    payload.validate()?;

    // The setlist first: a stranger must not learn which usernames exist
    // through this endpoint.
    let setlist = state
        .setlist_repo
        .find_by_id(setlist_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    require_verified_email(&state, user_id).await?;
    presets::limit(&presets::SETLIST_COLLABORATOR_CHANGES, user_id)?;

    let invitee = state
        .user_repo
        .find_by_username(&payload.username)
        .await?
        .ok_or_else(user_not_found)?;
    if invitee.status != Status::Active
        || active_ban(
            invitee.banned_at,
            invitee.banned_until,
            chrono::Utc::now().naive_utc(),
        )
        .is_some()
    {
        return Err(user_not_found());
    }

    let role = payload.role.unwrap_or(CollaboratorRole::Editor);
    state
        .setlist_collaborator_repo
        .invite(setlist_id, user_id, invitee.id, role)
        .await?;

    let actor_username = state
        .user_repo
        .find_by_id(user_id)
        .await?
        .map(|u| u.username)
        .unwrap_or_default();
    notify(
        &state,
        Notification::setlist_invitation(
            invitee.id,
            setlist_id,
            &setlist.title,
            role,
            user_id,
            &actor_username,
        ),
    )
    .await;

    info!(%user_id, %setlist_id, invitee_id = %invitee.id, role = role.key(), "Setlist collaborator invited");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/api/v1/setlists/{id}/collaborators/{user_id}",
    tags = ["Setlists"],
    summary = "Change a collaborator's role.",
    description = "Works on pending invites too. The owner changes anyone; a `manager` only moves people between `viewer` and `editor`. Nobody changes their own role.",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("user_id" = Uuid, Path, description = "The collaborator")
    ),
    request_body = UpdateCollaboratorPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Role changed."),
        (status = 403, description = "The caller can't give or take this role."),
        (status = 404, description = "Setlist or collaborator not found.")
    )
)]
pub async fn update_setlist_collaborator(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, target_id)): Path<(Uuid, Uuid)>,
    Json(payload): Json<UpdateCollaboratorPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    presets::limit(&presets::SETLIST_COLLABORATOR_CHANGES, user_id)?;
    let previous = state
        .setlist_collaborator_repo
        .change_role(setlist_id, user_id, target_id, payload.role)
        .await?;
    info!(%user_id, %setlist_id, %target_id, from = previous.key(), to = payload.role.key(), "Setlist collaborator role changed");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/setlists/{id}/collaborators/{user_id}",
    tags = ["Setlists"],
    summary = "Remove a collaborator, withdraw an invite, or leave a setlist.",
    description = "The owner removes anyone; a `manager` only viewers and editors. A collaborator removing themselves leaves the setlist. The songs of their library a collaborator had added stay in the setlist, held by it (`held`: in no one's library; see `POST /setlists/{id}/songs/{song_id}/copy`).",
    params(
        ("id" = Uuid, Path, description = "The ID of the setlist"),
        ("user_id" = Uuid, Path, description = "The collaborator")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Removed."),
        (status = 403, description = "The caller can't remove this collaborator."),
        (status = 404, description = "Setlist or collaborator not found.")
    )
)]
pub async fn remove_setlist_collaborator(
    State(state): State<AppState>,
    access: AccessControl,
    Path((setlist_id, target_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    if target_id != user_id {
        presets::limit(&presets::SETLIST_COLLABORATOR_CHANGES, user_id)?;
    }
    state
        .setlist_collaborator_repo
        .remove(setlist_id, user_id, target_id)
        .await?;
    info!(%user_id, %setlist_id, %target_id, "Setlist collaborator removed");
    Ok(StatusCode::NO_CONTENT)
}
