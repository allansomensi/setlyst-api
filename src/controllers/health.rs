use axum::{http::StatusCode, response::IntoResponse};

/// Liveness probe: the process is up and serving HTTP. Deliberately does
/// not touch the database (see `/status` for dependency health), so a
/// database blip doesn't make an orchestrator restart healthy API pods.
pub async fn health_check() -> impl IntoResponse {
    StatusCode::OK
}
