use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

type Version = String;

#[derive(Deserialize, Serialize, ToSchema)]
pub struct Database {
    pub version: Version,
    pub max_connections: i64,
    pub opened_connections: i64,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct Dependencies {
    pub database: Database,
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct Status {
    pub updated_at: NaiveDateTime,
    pub dependencies: Dependencies,
}
