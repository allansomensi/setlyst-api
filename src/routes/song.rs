use crate::{controllers::song, database::AppState};
use axum::{Router, routing::get};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(song::find_song_by_id)
                .patch(song::update_song)
                .delete(song::delete_song),
        )
        .route("/", get(song::find_all_songs).post(song::create_song))
        .with_state(state)
}
