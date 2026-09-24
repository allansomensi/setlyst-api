use crate::{controllers::backup, database::AppState, routes::MAX_IMPORT_BODY_BYTES};
use axum::{Router, extract::DefaultBodyLimit, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/export", get(backup::export_backup))
        // The only route that takes a large upload (a full backup).
        .route(
            "/import",
            axum::routing::post(backup::import_backup)
                .layer(DefaultBodyLimit::max(MAX_IMPORT_BODY_BYTES)),
        )
        .with_state(state)
}
