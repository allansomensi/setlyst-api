use crate::{
    controllers::user::{self, get_current_user, update_current_user},
    database::AppState,
};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/", get(user::find_all_users).post(user::create_user))
        .route("/me", get(get_current_user).patch(update_current_user))
        .route(
            "/{id}",
            get(user::find_user_by_id)
                .patch(user::update_user)
                .delete(user::delete_user),
        )
        .with_state(state)
}
