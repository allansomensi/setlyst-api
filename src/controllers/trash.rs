//! The trash: list, restore, delete permanently, empty.

use crate::{
    config::Config,
    database::{
        AppState,
        repositories::trash_repository::{TrashScopeFilter, not_in_trash},
    },
    errors::api_error::ApiError,
    models::{
        PaginatedResponse,
        auth::access::AccessControl,
        band::BandPermission,
        quota::QuotaLimits,
        resolve_page,
        trash::{
            EmptyTrashQuery, EmptyTrashResponse, TrashItem, TrashListQuery, TrashScope, TrashType,
        },
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::info;
use uuid::Uuid;

const ALL_TYPES: [TrashType; 5] = [
    TrashType::Song,
    TrashType::Artist,
    TrashType::Setlist,
    TrashType::Gig,
    TrashType::Tour,
];

/// Which kinds the caller may manage in a band's trash: `manage_songs`
/// covers songs and artists, `manage_setlists` setlists, gigs and tours.
/// `NotFound` for non-members, `Forbidden` with neither permission.
async fn band_types(
    state: &AppState,
    band_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<TrashType>, ApiError> {
    let role = state
        .band_repo
        .role_of(band_id, user_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let songs = state
        .band_repo
        .role_has_permission(band_id, role, BandPermission::ManageSongs)
        .await?;
    let setlists = state
        .band_repo
        .role_has_permission(band_id, role, BandPermission::ManageSetlists)
        .await?;
    let types: Vec<TrashType> = ALL_TYPES
        .into_iter()
        .filter(|t| match t.band_permission() {
            BandPermission::ManageSongs => songs,
            _ => setlists,
        })
        .collect();
    if types.is_empty() {
        return Err(ApiError::Forbidden);
    }
    Ok(types)
}

async fn scope_filter(
    state: &AppState,
    user_id: Uuid,
    scope: Option<TrashScope>,
    band_id: Option<Uuid>,
) -> Result<TrashScopeFilter, ApiError> {
    match scope.unwrap_or_default() {
        TrashScope::Personal => Ok(TrashScopeFilter {
            user_id,
            band_id: None,
            types: ALL_TYPES.to_vec(),
        }),
        TrashScope::Band => {
            let band_id = band_id.ok_or_else(|| {
                ApiError::BadRequest("`band_id` is required for the band trash.".to_string())
            })?;
            Ok(TrashScopeFilter {
                user_id,
                band_id: Some(band_id),
                types: band_types(state, band_id, user_id).await?,
            })
        }
    }
}

/// Checks the caller may manage this trashed row and returns the quota
/// limits of its scope (the caller's, or the band owner's).
async fn authorize_row(
    state: &AppState,
    user_id: Uuid,
    item_type: TrashType,
    id: Uuid,
) -> Result<Option<QuotaLimits>, ApiError> {
    let row = state
        .trash_repo
        .find(item_type, id)
        .await?
        .ok_or_else(not_in_trash)?;
    match row.band_id {
        None => {
            // Someone else's trash looks exactly like an empty one.
            if row.user_id != user_id {
                return Err(not_in_trash());
            }
            state.quota_repo.effective_limits(user_id).await
        }
        Some(band_id) => {
            let types = match band_types(state, band_id, user_id).await {
                Ok(types) => types,
                Err(ApiError::NotFound) => return Err(not_in_trash()),
                Err(e) => return Err(e),
            };
            if !types.contains(&item_type) {
                return Err(ApiError::Forbidden);
            }
            let owner: Option<Uuid> = sqlx::query_scalar(
                "SELECT user_id FROM band_members WHERE band_id = $1 AND role = 'owner' LIMIT 1",
            )
            .bind(band_id)
            .fetch_optional(&state.db)
            .await?;
            match owner {
                Some(owner) => state.quota_repo.effective_limits(owner).await,
                None => Ok(Some(state.quota_repo.get_defaults().await?)),
            }
        }
    }
}

fn parse_type(raw: &str) -> Result<TrashType, ApiError> {
    TrashType::parse(raw).ok_or_else(not_in_trash)
}

#[utoipa::path(
    get,
    path = "/api/v1/trash",
    tags = ["Trash"],
    summary = "List the trash.",
    description = "`scope=personal` (default): the caller's own content. `scope=band&band_id=`: a band's content; songs/artists need the band's `manage_songs` permission, setlists/gigs/tours `manage_setlists` — only the kinds the caller may manage are listed. Newest first. Songs deleted together with their artist are represented by the artist (`batch_count`). Items are purged for good at `purge_at`.",
    params(TrashListQuery),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Trash.", body = PaginatedResponse<TrashItem>),
        (status = 403, description = "No band permission covers the trash."),
        (status = 404, description = "Band not found, or the caller is not a member.")
    )
)]
pub async fn list_trash(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<TrashListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let scope = scope_filter(&state, user_id, query.scope, query.band_id).await?;
    let (items, total) = state
        .trash_repo
        .list(
            &scope,
            query.item_type,
            Config::get().trash_retention_days,
            page,
            per_page,
        )
        .await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/trash/{type}/{id}/restore",
    tags = ["Trash"],
    summary = "Restore an item from the trash.",
    description = "Brings back the item and everything deleted with it (an artist and its songs). Restoring a song whose artist is in the trash also restores that artist (only the artist, not its other songs). Quotas are checked again (`QUOTA_EXCEEDED`), and `RESTORE_CONFLICT` (409, `meta.type`) is returned when a live item now has the same name.",
    params(
        ("type" = TrashType, Path, description = "song, artist, setlist, gig or tour"),
        ("id" = Uuid, Path, description = "The item's ID")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Restored."),
        (status = 403, description = "Quota or band permission."),
        (status = 404, description = "Not in the trash (`NOT_IN_TRASH`)."),
        (status = 409, description = "Name conflict (`RESTORE_CONFLICT`).")
    )
)]
pub async fn restore_item(
    State(state): State<AppState>,
    access: AccessControl,
    Path((item_type, id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let item_type = parse_type(&item_type)?;
    let limits = authorize_row(&state, user_id, item_type, id).await?;
    state.trash_repo.restore(item_type, id, limits).await?;
    info!(%user_id, kind = item_type.key(), %id, "Restored from the trash");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/trash/{type}/{id}",
    tags = ["Trash"],
    summary = "Delete an item permanently.",
    description = "Only items already in the trash (`NOT_IN_TRASH` otherwise). Same permissions as restoring. An artist takes the songs deleted with it along.",
    params(
        ("type" = TrashType, Path, description = "song, artist, setlist, gig or tour"),
        ("id" = Uuid, Path, description = "The item's ID")
    ),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Deleted for good."),
        (status = 404, description = "Not in the trash.")
    )
)]
pub async fn delete_item(
    State(state): State<AppState>,
    access: AccessControl,
    Path((item_type, id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let item_type = parse_type(&item_type)?;
    authorize_row(&state, user_id, item_type, id).await?;
    state.trash_repo.delete_permanently(item_type, id).await?;
    info!(%user_id, kind = item_type.key(), %id, "Deleted permanently from the trash");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/trash",
    tags = ["Trash"],
    summary = "Empty the trash.",
    description = "Permanently deletes everything in the scope that the caller may manage (same rules as the listing). Returns `{ deleted }`.",
    params(EmptyTrashQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Emptied.", body = EmptyTrashResponse))
)]
pub async fn empty_trash(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<EmptyTrashQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let scope = scope_filter(&state, user_id, query.scope, query.band_id).await?;
    let deleted = state.trash_repo.empty(&scope).await?;
    info!(%user_id, band_id = ?scope.band_id, deleted, "Trash emptied");
    Ok(Json(EmptyTrashResponse { deleted }))
}
