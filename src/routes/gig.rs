use crate::{
    controllers::{gig, shared_file},
    database::AppState,
};
use axum::{Router, extract::DefaultBodyLimit, routing::get};

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
        .route("/{id}/export", get(shared_file::export_gig))
        // A gig file carries its songs' lyrics: a large upload.
        .route(
            "/import",
            axum::routing::post(shared_file::import_gig)
                .layer(DefaultBodyLimit::max(crate::routes::MAX_IMPORT_BODY_BYTES)),
        )
        .with_state(state)
}

/// Unauthenticated routes for viewing a gig via its public share token.
/// Mounted separately (outside the `authenticate` middleware) — see
/// `routes/mod.rs`.
pub fn create_public_routes(state: AppState) -> Router {
    // The answer carries the gig's setlist with lyrics: limited per client
    // like the public setlist view.
    Router::new()
        .route("/{token}", get(gig::get_public_gig))
        .layer(client_governor!(
            crate::routes::governor_presets::PUBLIC_SHARE_READ
        ))
        .with_state(state)
}
