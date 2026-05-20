use crate::{controllers::health, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(health::health_check))
        .with_state(state)
}
