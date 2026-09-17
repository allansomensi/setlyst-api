use crate::{controllers::notification, database::AppState};
use axum::{
    Router,
    routing::{delete, get, patch},
};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(notification::list_notifications))
        .route("/unread-count", get(notification::get_unread_count))
        .route(
            "/read-all",
            patch(notification::mark_all_notifications_read),
        )
        .route("/{id}/read", patch(notification::mark_notification_read))
        .route("/{id}", delete(notification::delete_notification))
        .with_state(state)
}
