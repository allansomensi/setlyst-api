use crate::{
    config::Config,
    database::AppState,
    models::status::{Database, Dependencies, ServiceHealth, Status},
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::Utc;
use std::time::Instant;
use tracing::warn;

/// Above this, the database is reported as degraded rather than operational.
const SLOW_QUERY_MS: u64 = 500;
/// Share of `max_connections` in use above which the database is degraded.
const CONNECTION_PRESSURE: f64 = 0.85;

async fn probe_database(state: &AppState) -> Database {
    let pool_size = state.db.size();
    let pool_idle = state.db.num_idle() as u32;

    let started = Instant::now();
    let version: Result<String, _> = sqlx::query_scalar("SHOW server_version;")
        .fetch_one(&state.db)
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;

    let Ok(version) = version else {
        warn!("Status probe: database unreachable");
        return Database {
            status: ServiceHealth::Down,
            version: None,
            latency_ms: None,
            max_connections: None,
            opened_connections: None,
            pool_size,
            pool_idle,
        };
    };

    let max_connections: Option<i64> = sqlx::query_scalar::<_, String>("SHOW max_connections;")
        .fetch_one(&state.db)
        .await
        .ok()
        .and_then(|v| v.parse().ok());

    let opened_connections: Option<i64> =
        sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1;")
            .bind(&Config::get().postgres_db)
            .fetch_one(&state.db)
            .await
            .ok();

    let under_pressure = match (opened_connections, max_connections) {
        (Some(open), Some(max)) if max > 0 => open as f64 / max as f64 >= CONNECTION_PRESSURE,
        _ => false,
    };

    Database {
        status: if latency_ms > SLOW_QUERY_MS || under_pressure {
            ServiceHealth::Degraded
        } else {
            ServiceHealth::Operational
        },
        // "16.4 (Debian 16.4-1.pgdg120+1)" → "16.4": the distro build
        // string is noise on a public status page.
        version: version.split_whitespace().next().map(str::to_string),
        latency_ms: Some(latency_ms),
        max_connections,
        opened_connections,
        pool_size,
        pool_idle,
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/status",
    tags = ["Status"],
    summary = "Platform health.",
    description = "Overall status, API version and uptime, and database health (latency and connection usage). Answers 503 when a dependency is down, with the same body.",
    responses(
        (status = 200, description = "Operational or degraded.", body = Status),
        (status = 503, description = "A dependency is down.", body = Status)
    )
)]
pub async fn show_status(State(state): State<AppState>) -> impl IntoResponse {
    let database = probe_database(&state).await;
    let overall = database.status;

    let body = Status {
        status: overall,
        updated_at: Utc::now().naive_utc(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        dependencies: Dependencies { database },
    };

    let code = if overall == ServiceHealth::Down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (code, Json(body))
}
