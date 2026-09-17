use crate::{controllers::user, database::AppState};
use axum::{
    Router,
    routing::{get, patch},
};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(user::find_all_users).post(user::create_user))
        .route(
            "/me",
            get(user::get_current_user).patch(user::update_current_user),
        )
        .route("/me/password", patch(user::change_current_user_password))
        .route(
            "/me/username-availability",
            get(user::check_username_availability),
        )
        .route(
            "/me/preferences",
            get(user::get_current_user_preferences).patch(user::update_current_user_preferences),
        )
        .route(
            "/{id}",
            get(user::find_user_by_id)
                .patch(user::update_user)
                .delete(user::delete_user),
        )
        .route("/{id}/username-history", get(user::get_username_history))
        .route("/{id}/profile", get(user::get_user_profile))
        .route("/{id}/preferences", get(user::get_user_preferences_by_id))
        .with_state(state)
}
