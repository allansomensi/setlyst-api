//! Items pinned to the home screen (`/users/me/pins`).

use crate::{
    database::{AppState, repositories::pin_repository::PinOutcome},
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        pin::{MAX_PINS, PinItemType, PinRef, Pinnable, PinnedItem, ReorderPinsPayload},
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

/// Sets `is_pinned` on every item of `items` for `user_id`.
pub async fn mark_pinned<T: Pinnable>(
    state: &AppState,
    user_id: Uuid,
    items: &mut [T],
) -> Result<(), ApiError> {
    if items.is_empty() {
        return Ok(());
    }
    let pinned = state.pin_repo.pinned_ids(user_id, T::PIN_TYPE).await?;
    for item in items.iter_mut() {
        let id = item.pin_id();
        item.set_pinned(pinned.contains(&id));
    }
    Ok(())
}

/// [`mark_pinned`] for one item.
pub async fn mark_one<T: Pinnable>(
    state: &AppState,
    user_id: Uuid,
    item: &mut T,
) -> Result<(), ApiError> {
    mark_pinned(state, user_id, std::slice::from_mut(item)).await
}

#[utoipa::path(
    get,
    path = "/api/v1/users/me/pins",
    tags = ["Users"],
    summary = "The caller's pinned items.",
    description = "In pin order. Items that were deleted (or are in the trash) or that the caller can no longer see are silently left out.",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Pinned items.", body = [PinnedItem]))
)]
pub async fn list_pins(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.pin_repo.list(access.user_id()).await?))
}

#[utoipa::path(
    put,
    path = "/api/v1/users/me/pins",
    tags = ["Users"],
    summary = "Pin an item to the home screen.",
    description = "Idempotent. The item must be one the caller can see. At most 12 pins (`QUOTA_EXCEEDED`, `meta: {resource: \"pins\", limit: 12}`).",
    request_body = PinRef,
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Pinned."),
        (status = 403, description = "Pin limit reached."),
        (status = 404, description = "Item not found.")
    )
)]
pub async fn pin_item(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<PinRef>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    if !state
        .pin_repo
        .is_accessible(user_id, payload.item_type, payload.item_id)
        .await?
    {
        return Err(ApiError::NotFound);
    }
    match state
        .pin_repo
        .pin(user_id, payload.item_type, payload.item_id)
        .await?
    {
        PinOutcome::Pinned => {
            info!(%user_id, item_id = %payload.item_id, "Item pinned");
            Ok(StatusCode::NO_CONTENT)
        }
        PinOutcome::AlreadyPinned => Ok(StatusCode::NO_CONTENT),
        PinOutcome::Full => Err(ApiError::quota_exceeded("pins", MAX_PINS)),
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/users/me/pins/{item_type}/{item_id}",
    tags = ["Users"],
    summary = "Unpin an item.",
    description = "Idempotent.",
    params(
        ("item_type" = PinItemType, Path, description = "setlist, band, song, tour or gig"),
        ("item_id" = Uuid, Path, description = "The item's ID")
    ),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Unpinned."))
)]
pub async fn unpin_item(
    State(state): State<AppState>,
    access: AccessControl,
    Path((item_type, item_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let item_type = PinItemType::parse(&item_type).ok_or(ApiError::NotFound)?;
    state
        .pin_repo
        .unpin(access.user_id(), item_type, item_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/api/v1/users/me/pins/order",
    tags = ["Users"],
    summary = "Reorder the pinned items.",
    description = "The listed pins come first, in that order; unknown entries are ignored and pins left out keep their relative order after them.",
    request_body = ReorderPinsPayload,
    security(("jwt_token" = [])),
    responses((status = 204, description = "Reordered."))
)]
pub async fn reorder_pins(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<ReorderPinsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    state
        .pin_repo
        .reorder(access.user_id(), &payload.items)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
