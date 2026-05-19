use crate::{controllers::backup, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/export", get(backup::export_backup))
        .route("/import", axum::routing::post(backup::import_backup))
        .with_state(state)
}
