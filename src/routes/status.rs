use crate::{controllers::status, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(status::show_status))
        .with_state(state)
}
