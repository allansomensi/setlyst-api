//! The moderation queue (`/admin/moderation`): flags on avatars,
//! usernames and band logos, raised automatically or by user reports.

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::{ApiError, codes},
    models::{
        PaginatedResponse,
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        moderation::{
            FlagListQuery, ModerationFlag, ModerationStatus, ModerationSummary, ModerationTarget,
            RescanResponse, ResolveAction, ResolveFlagPayload,
        },
        notification::Notification,
        resolve_page,
        user::Role,
    },
    moderation,
    services::{account::random_free_username, notifier::notify},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use tracing::info;
use uuid::Uuid;
use validator::Validate;

#[utoipa::path(
    get,
    path = "/api/v1/admin/moderation/flags",
    tags = ["Moderation"],
    summary = "The moderation queue (staff).",
    description = "Filter by `status` (`open` by default, `dismissed`, `actioned`, `all`), `target_type` and `user_id` (the account the flag is about). `current_value` shows the avatar, username or logo as it is now, to tell whether it changed since the flag.",
    params(FlagListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Flags.", body = PaginatedResponse<ModerationFlag>))
)]
pub async fn list_flags(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<FlagListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let (items, total) = state.moderation_repo.list(&query, page, per_page).await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/moderation/summary",
    tags = ["Moderation"],
    summary = "Open flags by type (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Summary.", body = ModerationSummary))
)]
pub async fn summary(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.moderation_repo.summary().await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/moderation/flags/{id}/resolve",
    tags = ["Moderation"],
    summary = "Resolve a flag (staff).",
    description = "`dismiss` closes it; `remove_avatar`, `remove_band_logo` and `reset_username` (a random `user-xxxxxx` name the owner may change right away) act on the account and close every open flag on the same value. Moderators can't act on staff accounts (`INSUFFICIENT_ROLE`). With `notify_user`, the owner gets a `moderation_action` notification. A flag that is no longer open answers `FLAG_ALREADY_RESOLVED` (409).",
    params(("id" = Uuid, Path, description = "Flag UUID")),
    request_body = ResolveFlagPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Resolved.", body = ModerationFlag),
        (status = 400, description = "Action doesn't fit the flag."),
        (status = 409, description = "The flag is already resolved (`FLAG_ALREADY_RESOLVED`)."),
        (status = 403, description = "Target outranks the caller."),
    )
)]
pub async fn resolve_flag(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<ResolveFlagPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    payload.validate()?;
    let flag = state
        .moderation_repo
        .find(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if flag.status != ModerationStatus::Open {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::FLAG_ALREADY_RESOLVED,
            "This flag is already resolved.",
        ));
    }
    // Acting on an account (even dismissing a flag about it) follows the
    // staff hierarchy: moderators handle regular users only.
    if flag.user.id != access.user_id() && !access.role().outranks(&flag.user.role) {
        return Err(ApiError::insufficient_role(match access.role() {
            Role::Moderator => "Moderators can only act on regular users.",
            _ => "You can't act on an account with the same or a higher role.",
        }));
    }
    // A flag about the caller's own account is a conflict of interest: a
    // moderator could otherwise quietly dismiss reports about their own
    // avatar or name. Only an admin (nobody outranks them, so nobody
    // else could ever close it) may dismiss one; nobody acts on themselves.
    if flag.user.id == access.user_id()
        && (!access.is_admin() || payload.action != ResolveAction::Dismiss)
    {
        return Err(ApiError::cannot_target_self(
            "You can't take moderation actions on your own account.",
        ));
    }

    let note = payload
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let (status, action_id) = match payload.action {
        ResolveAction::Dismiss => (ModerationStatus::Dismissed, actions::MODERATION_DISMISSED),
        ResolveAction::RemoveAvatar => {
            if flag.target_type == ModerationTarget::BandLogo {
                return Err(ApiError::BadRequest(
                    "This flag is about a band logo.".into(),
                ));
            }
            state
                .user_repo
                .remove_avatar(flag.user.id, access.user_id())
                .await?;
            (
                ModerationStatus::Actioned,
                actions::MODERATION_AVATAR_REMOVED,
            )
        }
        ResolveAction::RemoveBandLogo => {
            let Some(band) = &flag.band else {
                return Err(ApiError::BadRequest(
                    "This flag is not about a band logo.".into(),
                ));
            };
            sqlx::query("UPDATE bands SET logo_url = NULL WHERE id = $1")
                .bind(band.id)
                .execute(&state.db)
                .await?;
            (
                ModerationStatus::Actioned,
                actions::MODERATION_BAND_LOGO_REMOVED,
            )
        }
        ResolveAction::ResetUsername => {
            let username = random_free_username(&state).await?;
            state
                .user_repo
                .force_username(flag.user.id, &username, access.user_id())
                .await?;
            (
                ModerationStatus::Actioned,
                actions::MODERATION_USERNAME_RESET,
            )
        }
    };

    let closed = state
        .moderation_repo
        .resolve(
            &flag,
            status,
            payload.action.resolution(),
            note,
            access.user_id(),
        )
        .await?;

    if payload.notify_user && payload.action != ResolveAction::Dismiss {
        notify(
            &state,
            Notification::moderation_action(
                flag.user.id,
                payload.action.resolution(),
                note,
                flag.band.as_ref().map(|b| (b.id, b.name.as_str())),
            ),
        )
        .await;
    }

    AuditEvent::by(&access, action_id)
        .target("user", flag.user.id, &flag.user.username)
        .meta(json!({
            "flag_id": flag.id,
            "target_type": flag.target_type,
            "value": flag.value,
            "band_id": flag.band.as_ref().map(|b| b.id),
            "note": note,
            "flags_closed": closed,
            "notified": payload.notify_user,
        }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(flag_id = %id, action = payload.action.resolution(), "Moderation flag resolved");

    Ok(Json(
        state
            .moderation_repo
            .find(id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/moderation/rescan",
    tags = ["Moderation"],
    summary = "Re-check every username, avatar and band logo (admin).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "New flags raised.", body = RescanResponse))
)]
pub async fn rescan(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let flagged = moderation::rescan_all(&state).await?;
    AuditEvent::by(&access, actions::MODERATION_RESCAN)
        .meta(json!({ "flagged": flagged }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(RescanResponse { flagged }))
}
