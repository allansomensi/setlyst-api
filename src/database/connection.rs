use crate::{config::Config, errors::api_error::ApiError};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::time::Duration;

/// Opens the connection pool with explicit production limits instead of
/// sqlx's defaults: a bounded pool (so a traffic spike queues instead of
/// exhausting Postgres' `max_connections`), a bounded wait for a free
/// connection, and idle/lifetime recycling so connections dropped by a
/// proxy or failover are replaced instead of erroring.
pub async fn create_pool() -> Result<PgPool, ApiError> {
    let config = Config::get();
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(10 * 60))
        .max_lifetime(Duration::from_secs(30 * 60))
        .connect(&config.database_url)
        .await?;
    Ok(pool)
}

/// Applies every pending migration. Runs at startup (unless disabled with
/// `RUN_MIGRATIONS=false`), so a deploy can never serve a binary whose
/// queries expect columns the database doesn't have yet.
pub async fn run_migrations(pool: &PgPool) -> Result<(), ApiError> {
    sqlx::migrate!("./src/database/migrations")
        .run(pool)
        .await
        .map_err(|e| ApiError::DatabaseError(e.into()))
}
