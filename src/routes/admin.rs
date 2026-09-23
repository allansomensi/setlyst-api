use crate::{controllers::admin, database::AppState};
use axum::{
    Router,
    routing::{get, patch, post},
};

/// Routes nested under `/admin` — the staff console.
pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .route("/bands", get(admin::list_bands))
        .route(
            "/bands/{id}",
            get(admin::get_band)
                .patch(admin::update_band)
                .delete(admin::delete_band),
        )
        .route("/bands/{id}/members", post(admin::add_band_member))
        .route(
            "/bands/{id}/members/{user_id}",
            patch(admin::update_band_member_role).delete(admin::remove_band_member),
        )
        .route(
            "/bands/{id}/transfer-ownership",
            post(admin::transfer_band_ownership),
        )
        .route("/songs", get(admin::list_songs))
        .route(
            "/songs/{id}",
            get(admin::get_song)
                .patch(admin::update_song)
                .delete(admin::delete_song),
        )
        .route("/setlists", get(admin::list_setlists))
        .route(
            "/setlists/{id}",
            get(admin::get_setlist)
                .patch(admin::update_setlist)
                .delete(admin::delete_setlist),
        )
        .route(
            "/setlists/{id}/share/revoke",
            post(admin::revoke_setlist_share),
        )
        .route(
            "/setlists/{id}/share/unlock",
            post(admin::unlock_setlist_share),
        )
        .route("/gigs/{id}/share/revoke", post(admin::revoke_gig_share))
        .route("/gigs/{id}/share/unlock", post(admin::unlock_gig_share))
        .route("/shared-links", get(admin::list_shared_links))
        .route("/audit-logs", get(admin::list_audit_logs))
        .route(
            "/settings/quotas",
            get(admin::get_quota_defaults).put(admin::update_quota_defaults),
        )
        .with_state(state)
}
