use crate::{controllers::tour, database::AppState};
use axum::{Router, routing::get};

/// Routes nested under `/tours`.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(tour::find_all_tours).post(tour::create_tour))
        .route(
            "/{id}",
            get(tour::find_tour_by_id)
                .patch(tour::update_tour)
                .delete(tour::delete_tour),
        )
        .with_state(state)
}
