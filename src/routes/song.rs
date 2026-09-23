use crate::{controllers::song, database::AppState};
use axum::{
    Router,
    routing::{get, patch, post},
};

pub fn create_routes(state: AppState) -> Router {
    axum::Router::new()
        .route("/export/chordpro", get(song::export_songs_chordpro))
        .route("/import/chordpro", post(song::import_chordpro))
        .route("/tags", get(song::list_song_tags))
        .route(
            "/tags/{tag}",
            patch(song::rename_song_tag).delete(song::delete_song_tag),
        )
        .route("/{id}/setlists", get(song::find_song_setlists))
        .route("/{id}/export/pdf", get(song::export_song_pdf))
        .route("/{id}/export/chordpro", get(song::export_song_chordpro))
        .route(
            "/{id}",
            get(song::find_song_by_id)
                .patch(song::update_song)
                .delete(song::delete_song),
        )
        .route("/", get(song::find_all_songs).post(song::create_song))
        .with_state(state)
}
