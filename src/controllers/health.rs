use axum::{http::StatusCode, response::IntoResponse};

/// Liveness probe to check if the API web server is running.
/// Does not check external dependencies.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    tags = ["Status"],
    summary = "Health check (Liveness)",
    description = "A lightweight endpoint to verify if the API is running.",
    responses(
        (status = 200, description = "API is healthy", body = String)
    )
)]
pub async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}
