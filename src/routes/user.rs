use crate::{
    controllers::user::{
        change_current_user_password, create_user, delete_user, find_all_users, find_user_by_id,
        get_current_user, get_current_user_preferences, get_user_preferences_by_id,
        update_current_user, update_current_user_preferences, update_user,
    },
    database::AppState,
};
use axum::{
    Router,
    routing::{get, patch},
};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(find_all_users).post(create_user))
        .route("/me", get(get_current_user).patch(update_current_user))
        .route("/me/password", patch(change_current_user_password))
        .route(
            "/me/preferences",
            get(get_current_user_preferences).patch(update_current_user_preferences),
        )
        .route(
            "/{id}",
            get(find_user_by_id).patch(update_user).delete(delete_user),
        )
        .route("/{id}/preferences", get(get_user_preferences_by_id))
        .with_state(state)
}
