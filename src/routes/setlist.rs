use crate::{controllers::setlist, database::AppState};
use axum::{Router, routing::get};

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
        .with_state(state)
}
