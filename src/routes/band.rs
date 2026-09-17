use crate::{
    controllers::{band, gig, setlist},
    database::AppState,
};
use axum::{Router, routing::post};

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
        .with_state(state)
}

/// Routes nested under `/invites`, kept separate since they aren't scoped
/// to a specific band in the URL (the invite code resolves the band).
pub fn create_invite_routes(state: AppState) -> Router {
    Router::new()
        .route("/{code}/accept", post(band::accept_band_invite))
        .with_state(state)
}
