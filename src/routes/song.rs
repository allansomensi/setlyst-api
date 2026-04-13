use crate::{controllers::song, database::AppState};
use axum::{Router, routing::get};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/count", get(song::count_songs))
        .route("/{id}", get(song::find_song_by_id))
        .route(
            "/",
            get(song::find_all_songs)
                .post(song::create_song)
                .put(song::update_song)
                .delete(song::delete_song),
        )
        .with_state(state)
}
