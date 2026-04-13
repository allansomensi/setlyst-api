pub mod connection;
pub mod repositories;

use sqlx::PgPool;

pub struct AppState {
    pub db: PgPool,
}
