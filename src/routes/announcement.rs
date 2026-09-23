use crate::{controllers::announcement, database::AppState};
use axum::{
    Router,
    routing::{get, post},
};

/// Routes nested under `/announcements` (the caller's announcements).
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(announcement::list_for_user))
        .route("/active", get(announcement::active_for_user))
        .route("/{id}/seen", post(announcement::mark_seen))
        .route("/{id}/dismiss", post(announcement::dismiss))
        .route("/{id}/acknowledge", post(announcement::acknowledge))
        .with_state(state)
}
