use crate::{
    controllers::public, database::AppState, middlewares::client_ip::ClientIpKeyExtractor,
};
use axum::{Router, routing::get};
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

/// Public platform routes (no session), with their full `/public/...`
/// paths so they can be merged next to the public share routes.
pub fn create_routes(state: AppState) -> Router {
    // Unsubscribing: one per second, bursts of 10, per client IP.
    let unsubscribe = GovernorConfigBuilder::default()
        .per_second(1)
        .burst_size(10)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    let unsubscribe_routes = Router::new()
        .route(
            "/public/email/unsubscribe",
            get(public::inspect_unsubscribe).post(public::unsubscribe),
        )
        .layer(GovernorLayer::new(unsubscribe));

    Router::new()
        .route("/public/legal/version", get(public::legal_version))
        .route("/public/plans", get(public::list_plans))
        .route("/public/release-notes", get(public::list_release_notes))
        .merge(unsubscribe_routes)
        .with_state(state)
}
