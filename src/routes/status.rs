use crate::{controllers::status, database::AppState};
use axum::{Router, routing::get};
use std::sync::Arc;

pub fn create_routes(state: Arc<AppState>) -> Router {
    Router::new().route("/", get(status::show_status).with_state(state))
}
