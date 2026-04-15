use crate::{
    config::Config,
    database::AppState,
    errors::api_error::ApiError,
    models::status::{Database, Dependencies, Status},
};
use axum::{Json, extract::State, response::IntoResponse};
use chrono::Utc;
use tracing::info;

/// Retrieves the current status of the API, including the database connection status.
/// Provides information on the database version, maximum connections, and currently open connections.
/// Useful for health checks and monitoring API dependencies.
#[utoipa::path(
    get,
    path = "/api/v1/status",
    tags = ["Status"],
    summary = "Get API and database status",
    description = "Fetches the current operational status of the API, including database information such as version, max connections, and active connections.",
    responses(
        (status = 200, description = "Status retrieved successfully", body = Status)
    )
)]
pub async fn show_status(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let version: String = sqlx::query_scalar(r#"SHOW server_version;"#)
        .fetch_one(&state.db)
        .await?;

    let max_connections: i64 = sqlx::query_scalar::<_, String>(r#"SHOW max_connections;"#)
        .fetch_one(&state.db)
        .await?
        .parse()
        .expect("Error parsing max_connections as i64");

    let config = Config::get();

    let opened_connections: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM pg_stat_activity WHERE datname = $1;"#)
            .bind(&config.postgres_db)
            .fetch_one(&state.db)
            .await?;

    let database = Database {
        version,
        max_connections,
        opened_connections,
    };

    info!("Status queried");
    Ok(Json(Status {
        updated_at: Utc::now().naive_utc(),
        dependencies: Dependencies { database },
    }))
}
