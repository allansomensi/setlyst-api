use crate::{controllers::auth, database::AppState};
use axum::{Router, routing::post};
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

pub fn create_routes(state: AppState) -> Router {
    let governor_conf = GovernorConfigBuilder::default()
        .per_second(2)
        .burst_size(5)
        .finish()
        .unwrap();

    Router::new()
        .route("/login", post(auth::login))
        .route("/register", post(auth::register))
        .layer(GovernorLayer::new(governor_conf))
        .route("/verify", post(auth::verify))
        .with_state(state)
}
