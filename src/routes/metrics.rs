use crate::{controllers::metrics, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(metrics::get_metrics))
        .with_state(state)
}
