pub mod admin;
pub mod artist;
pub mod auth;
pub mod backup;
pub mod band;
pub mod gig;
pub mod health;
pub mod metrics;
pub mod migrations;
pub mod notification;
pub mod setlist;
pub mod song;
pub mod status;
pub mod swagger;
pub mod user;

use crate::{config::Config, database::AppState, middlewares::authentication::authenticate};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, StatusCode, header},
    middleware,
};
use std::time::Duration;
use tower_governor::{
    GovernorLayer, governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor,
};
use tower_http::{
    compression::CompressionLayer, set_header::SetResponseHeaderLayer, timeout::TimeoutLayer,
};

/// Largest accepted request body. Sized for the biggest legitimate
/// payload — a full backup import — everything else is far smaller and
/// additionally bounded by per-field validation.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// The authenticated API (everything that requires a bearer token).
fn protected_routes(state: AppState) -> Router {
    Router::new()
        .nest("/users", user::create_routes(state.clone()))
        .nest("/artists", artist::create_routes(state.clone()))
        .nest("/songs", song::create_routes(state.clone()))
        .nest("/setlists", setlist::create_routes(state.clone()))
        .nest("/gigs", gig::create_routes(state.clone()))
        .nest("/bands", band::create_routes(state.clone()))
        .nest("/invites", band::create_invite_routes(state.clone()))
        .nest("/notifications", notification::create_routes(state.clone()))
        .nest("/migrations", migrations::create_routes(state.clone()))
        .nest("/metrics", metrics::create_routes(state.clone()))
        .nest("/backup", backup::create_routes(state.clone()))
        .nest("/admin", admin::create_routes(state.clone()))
        .layer(middleware::from_fn_with_state(state, authenticate))
}

/// The API routes without the global rate limiter — what integration
/// tests drive directly (the IP-keyed governor needs real peer addresses).
pub fn api_router(state: AppState) -> Router {
    Router::new().nest(
        "/api/v1",
        protected_routes(state.clone())
            .nest("/auth", auth::create_routes(state.clone()))
            .nest("/status", status::create_routes(state.clone()))
            .nest("/health", health::create_routes())
            .nest(
                "/public/setlists",
                setlist::create_public_routes(state.clone()),
            )
            .nest("/public/gigs", gig::create_public_routes(state)),
    )
}

pub fn create_routes(state: AppState) -> Router {
    let global_governor_conf = GovernorConfigBuilder::default()
        .per_millisecond(25)
        .burst_size(300)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .unwrap();

    api_router(state)
        .merge(swagger::swagger_routes())
        .layer(Config::cors())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(30),
        ))
        .layer(CompressionLayer::new())
        // Conservative security headers. The API only serves JSON, PDFs and
        // the Swagger UI, none of which should ever be sniffed or framed.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("cross-origin"),
        ))
        .layer(GovernorLayer::new(global_governor_conf))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}
