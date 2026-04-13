use crate::{controllers::migrations, database::AppState};
use axum::{routing::get, Router};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(migrations::dry_run).post(migrations::live_run))
        .with_state(state)
}
