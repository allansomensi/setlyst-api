use crate::{controllers::auth, database::AppState};
use axum::{Router, routing::post};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .route("/verify", post(auth::verify))
        .with_state(state)
}
