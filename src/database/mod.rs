pub mod connection;
pub mod repositories;

use repositories::user_repository::UserRepository;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub user_repo: Arc<dyn UserRepository>,
}
