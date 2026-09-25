use crate::{controllers::public, database::AppState};
use axum::{
    Router,
    routing::{get, post},
};
use std::time::Duration;

/// Public platform routes (no session), with their full `/public/...`
/// paths so they can be merged next to the public share routes.
pub fn create_routes(state: AppState) -> Router {
    // Unsubscribing (the page and the providers' one-click POST): one per
    // second, bursts of 10, per client.
    let unsubscribe_routes = Router::new()
        .route(
            "/public/email/unsubscribe",
            get(public::inspect_unsubscribe).post(public::unsubscribe),
        )
        .route(
            "/public/email/unsubscribe/one-click",
            post(public::unsubscribe_one_click),
        )
        .layer(client_governor!(Duration::from_secs(1), 10));

    Router::new()
        .route("/public/legal/version", get(public::legal_version))
        .route("/public/billing", get(public::billing_mode))
        .route("/public/plans", get(public::list_plans))
        .route("/public/release-notes", get(public::list_release_notes))
        .merge(unsubscribe_routes)
        .with_state(state)
}
