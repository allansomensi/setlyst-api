use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ServiceHealth {
    /// Responding normally.
    Operational,
    /// Responding, but slowly or close to capacity.
    Degraded,
    /// Not responding.
    Down,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct Database {
    pub status: ServiceHealth,
    /// Server version, e.g. "16.4". `None` when unreachable.
    pub version: Option<String>,
    /// Round-trip time of a trivial query, in milliseconds.
    pub latency_ms: Option<u64>,
    pub max_connections: Option<i64>,
    pub opened_connections: Option<i64>,
    /// Connections currently held by this API instance's pool.
    pub pool_size: u32,
    pub pool_idle: u32,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct Dependencies {
    pub database: Database,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct Status {
    /// Overall platform health — the worst of the API and its dependencies.
    pub status: ServiceHealth,
    pub updated_at: NaiveDateTime,
    /// API version (from Cargo.toml).
    pub version: String,
    pub uptime_seconds: u64,
    pub dependencies: Dependencies,
}

/// The public status: health and version only. Infrastructure details
/// are staff-only (see [`Status`]).
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct PublicStatus {
    pub status: ServiceHealth,
    pub version: String,
}
