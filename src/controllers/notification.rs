use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationMeta, auth::access::AccessControl,
        notification::UnreadCountResponse,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use tracing::{debug, error, info};
use utoipa::IntoParams;
use uuid::Uuid;

#[derive(Deserialize, IntoParams, Debug)]
#[into_params(parameter_in = Query)]
pub struct NotificationQuery {
    #[param(default = 1, minimum = 1, required = false)]
    pub page: Option<i64>,
    #[param(default = 20, minimum = 1, maximum = 100, required = false)]
    pub per_page: Option<i64>,
    /// When `true`, only unread notifications are returned.
    #[param(required = false)]
    pub unread_only: Option<bool>,
}

#[utoipa::path(
    get,
    path = "/api/v1/notifications",
    tags = ["Notifications"],
    summary = "List the current user's notifications",
    description = "Returns a paginated list of the authenticated user's notifications, newest first. Only ever returns notifications addressed to the caller.",
    security(
        (),
        ("jwt_token" = [])
    ),
    params(
        NotificationQuery
    ),
    responses(
        (status = 200, description = "Notifications listed successfully"),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn list_notifications(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<NotificationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    let (current_page, per_page) = crate::models::resolve_page(query.page, query.per_page, 20);
    let unread_only = query.unread_only.unwrap_or(false);

    debug!(%user_id, current_page, per_page, unread_only, "Processing request to list notifications");

    match state
        .notification_repo
        .list_for_user(user_id, current_page, per_page, unread_only)
        .await
    {
        Ok((notifications, total_items)) => {
            let total_pages = (total_items as f64 / per_page as f64).ceil() as i64;

            info!(%user_id, total_items, "Notifications retrieved successfully");

            Ok(Json(PaginatedResponse {
                data: notifications,
                meta: PaginationMeta {
                    total_items,
                    current_page,
                    per_page,
                    total_pages,
                },
            }))
        }
        Err(e) => {
            error!(%user_id, error = %e, "Failed to retrieve notifications");
            Err(e)
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/notifications/unread-count",
    tags = ["Notifications"],
    summary = "Get the current user's unread notification count",
    description = "Returns how many of the authenticated user's notifications are unread. Meant to be polled cheaply to drive a badge.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Unread count retrieved successfully", body = UnreadCountResponse),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn get_unread_count(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    let unread_count = state.notification_repo.count_unread(user_id).await?;

    Ok(Json(UnreadCountResponse { unread_count }))
}

#[utoipa::path(
    patch,
    path = "/api/v1/notifications/{id}/read",
    tags = ["Notifications"],
    summary = "Mark a notification as read",
    description = "Marks a single notification belonging to the caller as read. Scoped to the caller — it is not possible to mark, or even detect the existence of, another user's notification.",
    params(
        ("id" = Uuid, Path, description = "Notification UUID")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "Notification marked as read"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Notification not found")
    )
)]
pub async fn mark_notification_read(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    debug!(%user_id, notification_id = %id, "Processing request to mark notification as read");

    state.notification_repo.mark_read(id, user_id).await?;

    info!(%user_id, notification_id = %id, "Notification marked as read");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch,
    path = "/api/v1/notifications/read-all",
    tags = ["Notifications"],
    summary = "Mark every notification as read",
    description = "Marks every unread notification belonging to the caller as read.",
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "Notifications marked as read"),
        (status = 401, description = "Unauthorized")
    )
)]
pub async fn mark_all_notifications_read(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    let updated = state.notification_repo.mark_all_read(user_id).await?;

    info!(%user_id, updated, "All notifications marked as read");
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/api/v1/notifications/{id}",
    tags = ["Notifications"],
    summary = "Delete a notification",
    description = "Deletes a single notification belonging to the caller.",
    params(
        ("id" = Uuid, Path, description = "Notification UUID")
    ),
    security(
        (),
        ("jwt_token" = [])
    ),
    responses(
        (status = 204, description = "Notification deleted"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Notification not found")
    )
)]
pub async fn delete_notification(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();

    state.notification_repo.delete(id, user_id).await?;

    info!(%user_id, notification_id = %id, "Notification deleted");
    Ok(StatusCode::NO_CONTENT)
}
