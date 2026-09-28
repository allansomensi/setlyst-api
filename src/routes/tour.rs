use crate::{
    controllers::{shared_file, tour},
    database::AppState,
};
use axum::{Router, extract::DefaultBodyLimit, routing::get};

/// Routes nested under `/tours`.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(tour::find_all_tours).post(tour::create_tour))
        .route("/{id}/export", get(shared_file::export_tour))
        // A tour file carries its songs' lyrics: a large upload.
        .route(
            "/import",
            axum::routing::post(shared_file::import_tour)
                .layer(DefaultBodyLimit::max(crate::routes::MAX_IMPORT_BODY_BYTES)),
        )
        .route(
            "/{id}",
            get(tour::find_tour_by_id)
                .patch(tour::update_tour)
                .delete(tour::delete_tour),
        )
        .with_state(state)
}
