use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        metrics::{MetricsResponse, TimeseriesQuery, TimeseriesResponse},
        user::Role,
    },
};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::{debug, error, info};

#[utoipa::path(
    get,
    path = "/api/v1/metrics",
    tags = ["Metrics"],
    summary = "Get dashboard metrics.",
    description = "Returns statistics for the authenticated user (artists, songs, setlists, etc.). \
                   Admin users receive global platform-wide metrics instead.",
    security(
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Metrics retrieved successfully.", body = MetricsResponse),
        (status = 401, description = "Unauthorized."),
        (status = 500, description = "An error occurred while retrieving metrics.")
    )
)]
pub async fn get_metrics(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let role = &access.0.role;

    debug!(
        %user_id,
        ?role,
        "Processing request to retrieve metrics"
    );

    if *role == Role::Admin {
        match state.metrics_repo.get_admin_metrics().await {
            Ok(metrics) => {
                info!(%user_id, "Admin metrics retrieved successfully");
                Ok((StatusCode::OK, Json(MetricsResponse::Admin(metrics))))
            }
            Err(e) => {
                error!(%user_id, error = %e, "Failed to retrieve admin metrics");
                Err(e)
            }
        }
    } else {
        match state.metrics_repo.get_user_metrics(user_id).await {
            Ok(metrics) => {
                info!(%user_id, "User metrics retrieved successfully");
                Ok((StatusCode::OK, Json(MetricsResponse::User(metrics))))
            }
            Err(e) => {
                error!(%user_id, error = %e, "Failed to retrieve user metrics");
                Err(e)
            }
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/metrics/timeseries",
    tags = ["Metrics"],
    summary = "Get daily activity time series for dashboard/analytics charts.",
    description = "Returns gap-filled daily counts for the authenticated user's own activity over the trailing `days` days (default 30, max 365). \\
                   Admin users receive platform-wide series (new registrations, songs, setlists, bands) instead.",
    params(TimeseriesQuery),
    security(
        ("jwt_token" = [])
    ),
    responses(
        (status = 200, description = "Time series retrieved successfully.", body = TimeseriesResponse),
        (status = 401, description = "Unauthorized."),
        (status = 500, description = "An error occurred while retrieving the time series.")
    )
)]
pub async fn get_timeseries_metrics(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<TimeseriesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = access.user_id();
    let role = &access.0.role;
    let days = query.days();

    debug!(
        %user_id,
        ?role,
        days,
        "Processing request to retrieve timeseries metrics"
    );

    if *role == Role::Admin {
        match state.metrics_repo.get_admin_timeseries(days).await {
            Ok(series) => {
                info!(%user_id, days, "Admin timeseries retrieved successfully");
                Ok((StatusCode::OK, Json(TimeseriesResponse::Admin(series))))
            }
            Err(e) => {
                error!(%user_id, error = %e, "Failed to retrieve admin timeseries");
                Err(e)
            }
        }
    } else {
        match state.metrics_repo.get_user_timeseries(user_id, days).await {
            Ok(series) => {
                info!(%user_id, days, "User timeseries retrieved successfully");
                Ok((StatusCode::OK, Json(TimeseriesResponse::User(series))))
            }
            Err(e) => {
                error!(%user_id, error = %e, "Failed to retrieve user timeseries");
                Err(e)
            }
        }
    }
}
