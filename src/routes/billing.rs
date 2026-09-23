use crate::{controllers::billing, database::AppState};
use axum::{
    Router,
    routing::{get, post},
};

/// Routes nested under `/billing` (the caller's plan, payments, credits
/// and referrals).
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/me", get(billing::get_me))
        .route("/redeem", post(billing::redeem_code))
        .route("/credits", get(billing::list_credits))
        .route("/credits/redeem", post(billing::redeem_reward))
        .route("/referrals", get(billing::list_referrals))
        .route("/history", get(billing::history))
        .route("/checkout", post(billing::checkout))
        .route("/subscription/change", post(billing::change_subscription))
        .route("/portal", post(billing::portal))
        .with_state(state)
}

/// Routes nested under `/webhooks`: called by the payment provider, so
/// outside authentication (each request is verified by its signature).
pub fn create_webhook_routes(state: AppState) -> Router {
    Router::new()
        .route("/stripe", post(billing::stripe_webhook))
        .with_state(state)
}
