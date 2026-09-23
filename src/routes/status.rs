use crate::{controllers::status, database::AppState, middlewares::authentication::authenticate};
use axum::{Router, middleware, routing::get};

/// `/status` is public (health and version only); `/status/details` needs
/// a staff session.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(status::show_status))
        .route(
            "/details",
            get(status::show_status_details)
                .route_layer(middleware::from_fn_with_state(state.clone(), authenticate)),
        )
        .with_state(state)
}
