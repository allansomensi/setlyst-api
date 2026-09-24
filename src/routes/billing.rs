use crate::{
    controllers::billing, database::AppState, middlewares::client_ip::ClientIpKeyExtractor,
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

/// Largest webhook body accepted. Stripe events are a few kilobytes; the
/// body is buffered before its signature is checked, so anything bigger
/// is refused up front.
const WEBHOOK_MAX_BODY_BYTES: usize = 256 * 1024;

/// Routes nested under `/billing` (the caller's plan, payments, credits
/// and referrals).
pub fn create_routes(state: AppState) -> Router {
    // Promo-code attempts per client IP, on top of the per-account limit:
    // many accounts from one address can't multiply the guesses. One every
    // 2 minutes, bursts of 10.
    let redeem = GovernorConfigBuilder::default()
        .period(Duration::from_secs(120))
        .burst_size(10)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    let redeem_routes = Router::new()
        .route("/redeem", post(billing::redeem_code))
        .layer(GovernorLayer::new(pruned!(redeem)));

    Router::new()
        .merge(redeem_routes)
        .route("/me", get(billing::get_me))
        .route("/credits", get(billing::list_credits))
        .route("/credits/redeem", post(billing::redeem_reward))
        .route("/referrals", get(billing::list_referrals))
        .route("/history", get(billing::history))
        .route("/checkout", post(billing::checkout))
        .route("/subscription/change", post(billing::change_subscription))
        .route("/withdraw", post(billing::withdraw))
        .route("/portal", post(billing::portal))
        .with_state(state)
}

/// Routes nested under `/webhooks`: called by the payment provider, so
/// outside authentication (each request is verified by its signature).
pub fn create_webhook_routes(state: AppState) -> Router {
    Router::new()
        .route("/stripe", post(billing::stripe_webhook))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .with_state(state)
}
