use crate::{
    config::Config,
    database::AppState,
    errors::api_error::ApiError,
    models::{
        auth::access::AccessControl,
        status::{Database, Dependencies, PublicStatus, ServiceHealth, Status},
    },
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::Utc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
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
        // "16.4 (Debian 16.4-1.pgdg120+1)" → "16.4".
        version: version.split_whitespace().next().map(str::to_string),
        latency_ms: Some(latency_ms),
        max_connections,
        opened_connections,
        pool_size,
        pool_idle,
    }
}

fn http_status(health: ServiceHealth) -> StatusCode {
    if health == ServiceHealth::Down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

/// How long the public status answer is reused.
const PUBLIC_STATUS_TTL: Duration = Duration::from_secs(10);

/// The last public probe: when it ran and what it found.
static PUBLIC_STATUS: Mutex<Option<(Instant, ServiceHealth)>> = Mutex::const_new(None);

/// The cheapest possible database check for the unauthenticated status:
/// one `SELECT 1`, timed.
async fn quick_probe(state: &AppState) -> ServiceHealth {
    let started = Instant::now();
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) if started.elapsed().as_millis() as u64 > SLOW_QUERY_MS => ServiceHealth::Degraded,
        Ok(_) => ServiceHealth::Operational,
        Err(e) => {
            warn!(error = %e, "Status probe: database unreachable");
            ServiceHealth::Down
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/v1/status",
    tags = ["Status"],
    summary = "Platform health.",
    description = "Overall status only (`operational`, `degraded` or `down`), refreshed at most every 10 seconds. Answers 503 when a dependency is down, with the same body. The API version and infrastructure details (database version, connections, uptime) are only available to staff at `/status/details`. For a load balancer's health check use `/health`, which never touches the database.",
    responses(
        (status = 200, description = "Operational or degraded.", body = PublicStatus),
        (status = 503, description = "A dependency is down.", body = PublicStatus)
    )
)]
pub async fn show_status(State(state): State<AppState>) -> impl IntoResponse {
    // Unauthenticated and cheap to call: one probe per 10 s per instance,
    // however often it's hit (concurrent callers wait for the same probe).
    let mut cached = PUBLIC_STATUS.lock().await;
    let health = match *cached {
        Some((at, health)) if at.elapsed() < PUBLIC_STATUS_TTL => health,
        _ => {
            let health = quick_probe(&state).await;
            *cached = Some((Instant::now(), health));
            health
        }
    };
    drop(cached);
    (http_status(health), Json(PublicStatus { status: health }))
}

#[utoipa::path(
    get,
    path = "/api/v1/status/details",
    tags = ["Status"],
    summary = "Detailed platform health (staff).",
    description = "Overall status, API version and uptime, and database health (version, latency and connection usage). Requires Admin or Moderator role.",
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Operational or degraded.", body = Status),
        (status = 403, description = "Not staff."),
        (status = 503, description = "A dependency is down.", body = Status)
    )
)]
pub async fn show_status_details(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let database = probe_database(&state).await;
    let overall = database.status;

    let body = Status {
        status: overall,
        updated_at: Utc::now().naive_utc(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        dependencies: Dependencies { database },
    };

    Ok((http_status(overall), Json(body)))
}
