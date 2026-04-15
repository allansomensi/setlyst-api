use crate::{controllers::user, database::AppState};
use axum::{Router, routing::get};

pub fn create_routes(state: AppState) -> Router {
    axum::Router::new()
        .route(
            "/{id}",
            get(user::find_user_by_id)
                .patch(user::update_user)
                .delete(user::delete_user),
        )
        .route("/", get(user::find_all_users).post(user::create_user))
        .with_state(state)
}
