use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::{auth::access::AccessControl, metrics::MetricsResponse, user::Role},
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
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
