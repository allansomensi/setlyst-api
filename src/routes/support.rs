use crate::{controllers::support, database::AppState};
use axum::{
    Router,
    routing::{get, post},
};

/// Routes nested under `/support`: the caller's own requests. The staff
/// inbox lives under `/admin/support`.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route(
            "/tickets",
            get(support::list_my_tickets).post(support::create_ticket),
        )
        .route("/summary", get(support::my_summary))
        .route("/tickets/{id}", get(support::get_my_ticket))
        .route("/tickets/{id}/messages", post(support::reply_my_ticket))
        .route("/tickets/{id}/close", post(support::close_my_ticket))
        .route("/tickets/{id}/rating", post(support::rate_my_ticket))
        .with_state(state)
}
