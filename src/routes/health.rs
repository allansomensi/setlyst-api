use crate::controllers::health;
use axum::{Router, routing::get};

pub fn create_routes() -> Router {
    Router::new().route("/", get(health::health_check))
}
