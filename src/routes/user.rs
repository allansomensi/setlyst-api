use crate::{controllers::user, database::AppState};
use axum::{routing::get, Router};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/count", get(user::count_users))
        .route("/{id}", get(user::find_user_by_id))
        .route(
            "/",
            get(user::find_all_users)
                .post(user::create_user)
                .put(user::update_user)
                .delete(user::delete_user),
        )
        .with_state(state)
}
