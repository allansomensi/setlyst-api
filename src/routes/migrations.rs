use crate::{controllers::migrations, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(migrations::dry_run).post(migrations::live_run))
        .with_state(state)
}
