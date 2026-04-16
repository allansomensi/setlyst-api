use crate::{
    controllers::setlist::{
        self, add_song_to_setlist, get_setlist_songs, remove_song_from_setlist,
        reorder_setlist_songs,
    },
    database::AppState,
};
use axum::{
    Router,
    routing::{delete, get, patch},
};

pub fn create_routes(state: AppState) -> Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(setlist::find_setlist_by_id)
                .patch(setlist::update_setlist)
                .delete(setlist::delete_setlist),
        )
        .route(
            "/",
            get(setlist::find_all_setlists).post(setlist::create_setlist),
        )
        .route(
            "/{id}/songs",
            get(get_setlist_songs).post(add_song_to_setlist),
        )
        .route("/{id}/songs/reorder", patch(reorder_setlist_songs))
        .route("/{id}/songs/{song_id}", delete(remove_song_from_setlist))
        .with_state(state)
}
