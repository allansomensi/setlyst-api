use crate::{
    controllers::{admin, announcement, billing, moderation, release_note},
    database::AppState,
};
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
        .merge(announcement_routes())
        .merge(release_note_routes())
        .merge(billing_routes())
        .merge(moderation_routes())
        .with_state(state)
}

/// `/admin/announcements`.
fn announcement_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/announcements",
            get(announcement::admin_list).post(announcement::admin_create),
        )
        .route(
            "/announcements/preview-audience",
            post(announcement::admin_preview_audience),
        )
        .route(
            "/announcements/{id}",
            get(announcement::admin_get)
                .patch(announcement::admin_update)
                .delete(announcement::admin_delete),
        )
        .route(
            "/announcements/{id}/publish",
            post(announcement::admin_publish),
        )
        .route(
            "/announcements/{id}/archive",
            post(announcement::admin_archive),
        )
}

/// `/admin/release-notes`.
fn release_note_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/release-notes",
            get(release_note::admin_list).post(release_note::admin_create),
        )
        .route(
            "/release-notes/{id}",
            get(release_note::admin_get)
                .patch(release_note::admin_update)
                .delete(release_note::admin_delete),
        )
        .route(
            "/release-notes/{id}/publish",
            post(release_note::admin_publish),
        )
        .route(
            "/release-notes/{id}/unpublish",
            post(release_note::admin_unpublish),
        )
}

/// Plans, subscriptions, credits, promo codes and promotions.
fn billing_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/billing/settings",
            get(billing::get_settings).put(billing::update_settings),
        )
        .route("/billing/overview", get(billing::overview))
        .route("/billing/grant-trials", post(billing::grant_trials))
        .route("/plans", get(billing::list_plans))
        .route(
            "/plans/{code}",
            get(billing::get_plan).put(billing::upsert_plan),
        )
        .route(
            "/users/{id}/subscription",
            get(billing::get_user_subscription)
                .put(billing::grant_user_subscription)
                .delete(billing::revoke_user_subscription),
        )
        .route("/users/{id}/credits", post(billing::adjust_user_credits))
        .route(
            "/promo-codes",
            get(billing::list_promo_codes).post(billing::create_promo_code),
        )
        .route("/promo-codes/{id}", patch(billing::update_promo_code))
        .route(
            "/promo-codes/{id}/redemptions",
            get(billing::promo_redemptions),
        )
        .route(
            "/promotions",
            get(billing::list_promotions).post(billing::create_promotion),
        )
        .route(
            "/promotions/{id}",
            patch(billing::update_promotion).delete(billing::delete_promotion),
        )
}

/// `/admin/moderation`.
fn moderation_routes() -> Router<AppState> {
    Router::new()
        .route("/moderation/flags", get(moderation::list_flags))
        .route("/moderation/summary", get(moderation::summary))
        .route(
            "/moderation/flags/{id}/resolve",
            post(moderation::resolve_flag),
        )
        .route("/moderation/rescan", post(moderation::rescan))
}
