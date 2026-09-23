use crate::{controllers::pin, database::AppState};
use axum::{
    Router,
    routing::{delete, get, put},
};

/// Routes nested under `/users/me/pins`. `/users/{id}` in the users router
/// never matches these (a static `me` segment wins over the `{id}`
/// parameter, and nothing else is registered under `/users/me/pins`).
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(pin::list_pins).put(pin::pin_item))
        .route("/order", put(pin::reorder_pins))
        .route("/{item_type}/{item_id}", delete(pin::unpin_item))
        .with_state(state)
}
