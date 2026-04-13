use crate::{controllers::artist, database::AppState};
use axum::routing::get;
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> axum::Router {
    axum::Router::new()
        .route("/count", get(artist::count_artists))
        .route("/{id}", get(artist::find_artist_by_id))
        .route(
            "/",
            get(artist::find_all_artists)
                .post(artist::create_artist)
                .put(artist::update_artist)
                .delete(artist::delete_artist),
        )
        .with_state(state)
}
