use crate::{config::Config, errors::api_error::ApiError};
use sqlx::{Executor, PgPool, postgres::PgPoolOptions};
use std::time::Duration;

/// Per-session limits set on every new connection (unless
/// `DATABASE_SKIP_SESSION_SETTINGS=true`):
///
/// - `statement_timeout`: a query cancelled on the Rust side (a dropped
///   request future) otherwise keeps running on the server;
/// - `idle_in_transaction_session_timeout`: a transaction abandoned half
///   way can't hold its locks (and a pooled connection) forever;
/// - `lock_timeout`: waiting on a row or advisory lock fails instead of
///   queueing indefinitely behind a stuck writer.
///
/// Heavier work raises the statement timeout locally (`SET LOCAL`) where
/// needed. Behind a transaction-mode pooler (Neon's `-pooler` endpoint,
/// PgBouncer), session settings don't stick to a client: set them on the
/// role instead (`ALTER ROLE app SET statement_timeout = '15s'` etc.) and
/// skip them here.
pub const SESSION_SETTINGS: &str = "SET statement_timeout = '15s';
     SET idle_in_transaction_session_timeout = '30s';
     SET lock_timeout = '5s'";

/// Opens the connection pool with explicit production limits instead of
/// sqlx's defaults: a bounded pool (so a traffic spike queues instead of
/// exhausting Postgres' `max_connections`), a bounded wait for a free
/// connection, idle/lifetime recycling so connections dropped by a
/// proxy or failover are replaced instead of erroring, and the session
/// timeouts of [`SESSION_SETTINGS`].
pub async fn create_pool() -> Result<PgPool, ApiError> {
    let config = Config::get();
    let skip_session_settings = config.database_skip_session_settings;
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(10 * 60))
        .max_lifetime(Duration::from_secs(30 * 60))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                if !skip_session_settings {
                    // A multi-statement string runs over the simple query
                    // protocol, in one round trip.
                    conn.execute(SESSION_SETTINGS).await?;
                }
                Ok(())
            })
        })
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
