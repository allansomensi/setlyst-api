use crate::{controllers::billing, database::AppState};
use axum::{
    Router,
    routing::{get, post},
};

/// Routes nested under `/billing` (the caller's plan, credits and
/// referrals).
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/me", get(billing::get_me))
        .route("/redeem", post(billing::redeem_code))
        .route("/credits", get(billing::list_credits))
        .route("/credits/redeem", post(billing::redeem_reward))
        .route("/referrals", get(billing::list_referrals))
        .route("/history", get(billing::history))
        .with_state(state)
}
