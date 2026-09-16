use crate::{controllers::gig, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(gig::find_gig_by_id)
                .patch(gig::update_gig)
                .delete(gig::delete_gig),
        )
        .route(
            "/{id}/share",
            axum::routing::post(gig::enable_gig_sharing).delete(gig::disable_gig_sharing),
        )
        .route("/", get(gig::find_all_gigs).post(gig::create_gig))
        .with_state(state)
}

/// Unauthenticated routes for viewing a gig via its public share token.
/// Mounted separately (outside the `authenticate` middleware) — see
/// `routes/mod.rs`.
pub fn create_public_routes(state: AppState) -> Router {
    Router::new()
        .route("/{token}", get(gig::get_public_gig))
        .with_state(state)
}
