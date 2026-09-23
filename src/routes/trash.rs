use crate::{controllers::trash, database::AppState};
use axum::{
    Router,
    routing::{delete, get, post},
};

/// Routes nested under `/trash`.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(trash::list_trash).delete(trash::empty_trash))
        .route("/{type}/{id}/restore", post(trash::restore_item))
        .route("/{type}/{id}", delete(trash::delete_item))
        .with_state(state)
}
