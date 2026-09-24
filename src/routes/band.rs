use crate::{
    controllers::{band, band_note, gig, setlist, suggestion, tour},
    database::AppState,
    middlewares::client_ip::ClientIpKeyExtractor,
};
use axum::{Router, routing::post};
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

/// Routes nested under `/bands`.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            axum::routing::get(band::find_all_bands).post(band::create_band),
        )
        .route(
            "/{id}",
            axum::routing::get(band::find_band_by_id)
                .patch(band::update_band)
                .delete(band::delete_band),
        )
        .route("/{id}/transfer-ownership", post(band::transfer_ownership))
        .route(
            "/{id}/favorite",
            axum::routing::post(band::favorite_band).delete(band::unfavorite_band),
        )
        .route("/{id}/members", axum::routing::get(band::list_band_members))
        .route(
            "/{id}/members/{user_id}",
            axum::routing::patch(band::update_band_member_role).delete(band::remove_band_member),
        )
        .route(
            "/{id}/members/{user_id}/title",
            axum::routing::patch(band::update_band_member_title),
        )
        .route(
            "/{id}/invites",
            axum::routing::get(band::list_band_invites).post(band::create_band_invite),
        )
        .route(
            "/{id}/invites/{invite_id}",
            axum::routing::delete(band::revoke_band_invite),
        )
        .route(
            "/{id}/permissions",
            axum::routing::get(band::get_band_role_permissions)
                .put(band::update_band_role_permissions),
        )
        .route(
            "/{id}/setlists",
            axum::routing::get(setlist::find_band_setlists),
        )
        .route("/{id}/gigs", axum::routing::get(gig::find_band_gigs))
        .route(
            "/{id}/repertoire",
            axum::routing::get(setlist::find_band_repertoire),
        )
        .route("/{id}/tours", axum::routing::get(tour::find_band_tours))
        .route(
            "/{id}/suggestions",
            axum::routing::get(suggestion::list_suggestions).post(suggestion::create_suggestion),
        )
        .route(
            "/{id}/suggestions/{sid}/vote",
            axum::routing::put(suggestion::vote_suggestion).delete(suggestion::unvote_suggestion),
        )
        .route(
            "/{id}/suggestions/{sid}/accept",
            post(suggestion::accept_suggestion),
        )
        .route(
            "/{id}/suggestions/{sid}/reject",
            post(suggestion::reject_suggestion),
        )
        .route(
            "/{id}/suggestions/{sid}/withdraw",
            post(suggestion::withdraw_suggestion),
        )
        .route(
            "/{id}/notes",
            axum::routing::get(band_note::list_notes).post(band_note::create_note),
        )
        .route(
            "/{id}/notes/{nid}",
            axum::routing::patch(band_note::update_note).delete(band_note::delete_note),
        )
        .with_state(state)
}

/// Routes nested under `/invites`, kept separate since they aren't scoped
/// to a specific band in the URL (the invite code resolves the band).
pub fn create_invite_routes(state: AppState) -> Router {
    // Invite codes are long, but guessing is still throttled: one attempt
    // every 2 seconds per client IP, bursts of 10.
    let governor = GovernorConfigBuilder::default()
        .period(Duration::from_secs(2))
        .burst_size(10)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    Router::new()
        .route("/{code}/accept", post(band::accept_band_invite))
        .layer(GovernorLayer::new(pruned!(governor)))
        .with_state(state)
}
