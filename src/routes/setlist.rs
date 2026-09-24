use crate::{
    controllers::setlist, database::AppState, middlewares::client_ip::ClientIpKeyExtractor,
};
use axum::{
    Router,
    routing::{delete, get, patch},
};
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

pub fn create_routes(state: AppState) -> Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(setlist::find_setlist_by_id)
                .patch(setlist::update_setlist)
                .delete(setlist::delete_setlist),
        )
        .route("/{id}/export/pdf", get(setlist::export_setlist_pdf))
        .route(
            "/{id}/share",
            axum::routing::post(setlist::enable_setlist_sharing)
                .delete(setlist::disable_setlist_sharing),
        )
        .route(
            "/{id}/favorite",
            axum::routing::post(setlist::favorite_setlist).delete(setlist::unfavorite_setlist),
        )
        .route(
            "/",
            get(setlist::find_all_setlists).post(setlist::create_setlist),
        )
        .route(
            "/{id}/songs",
            get(setlist::get_setlist_songs).post(setlist::add_song_to_setlist),
        )
        .route("/{id}/songs/reorder", patch(setlist::reorder_setlist_songs))
        .route(
            "/{id}/songs/{song_id}",
            delete(setlist::remove_song_from_setlist),
        )
        .route(
            "/{id}/duplicate",
            axum::routing::post(setlist::duplicate_setlist),
        )
        .route("/{id}/items", get(setlist::get_setlist_items))
        .route("/{id}/items/reorder", patch(setlist::reorder_setlist_items))
        .route(
            "/{id}/blocks",
            axum::routing::post(setlist::create_setlist_block),
        )
        .route(
            "/{id}/blocks/{marker_id}",
            patch(setlist::update_setlist_block),
        )
        .route(
            "/{id}/breaks",
            axum::routing::post(setlist::create_setlist_break),
        )
        .route(
            "/{id}/breaks/{marker_id}",
            patch(setlist::update_setlist_break),
        )
        .route(
            "/{id}/markers/{marker_id}",
            delete(setlist::delete_setlist_marker),
        )
        .with_state(state)
}

/// Unauthenticated routes for viewing/exporting a setlist via its public
/// share token. Mounted separately (outside the `authenticate` middleware) —
/// see `routes/mod.rs`.
pub fn create_public_routes(state: AppState) -> Router {
    // Anonymous PDF rendering is expensive: one per second per client IP,
    // bursts of 5 (on top of the global PDF concurrency limit).
    let pdf_governor = GovernorConfigBuilder::default()
        .per_second(1)
        .burst_size(5)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    let pdf = Router::new()
        .route(
            "/{token}/export/pdf",
            get(setlist::export_public_setlist_pdf),
        )
        .layer(GovernorLayer::new(pruned!(pdf_governor)));

    Router::new()
        .route("/{token}", get(setlist::get_public_setlist))
        .merge(pdf)
        .with_state(state)
}
