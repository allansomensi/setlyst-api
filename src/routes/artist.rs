use crate::{controllers::artist, database::AppState};
use axum::routing::get;

pub fn create_routes(state: AppState) -> axum::Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(artist::find_artist_by_id)
                .patch(artist::update_artist)
                .delete(artist::delete_artist),
        )
        .route(
            "/",
            get(artist::find_all_artists).post(artist::create_artist),
        )
        .with_state(state)
}
