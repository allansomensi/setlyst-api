pub mod artist;
pub mod auth;
pub mod backup;
pub mod metrics;
pub mod migrations;
pub mod setlist;
pub mod song;
pub mod status;
pub mod swagger;
pub mod user;

use crate::{config::Config, database::AppState, middlewares::authentication::authenticate};
use axum::{Router, extract::DefaultBodyLimit, middleware};
use tower_governor::{
    GovernorLayer, governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor,
};

pub fn create_routes(state: AppState) -> Router {
    let global_governor_conf = GovernorConfigBuilder::default()
        .per_millisecond(200)
        .burst_size(60)
        .key_extractor(SmartIpKeyExtractor)
        .finish()
        .unwrap();

    Router::new()
        .nest(
            "/api/v1",
            Router::new()
                .nest("/users", user::create_routes(state.clone()))
                .nest("/artists", artist::create_routes(state.clone()))
                .nest("/songs", song::create_routes(state.clone()))
                .nest("/setlists", setlist::create_routes(state.clone()))
                .nest("/migrations", migrations::create_routes(state.clone()))
                .nest("/metrics", metrics::create_routes(state.clone()))
                .nest("/backup", backup::create_routes(state.clone()))
                .layer(middleware::from_fn(authenticate))
                .nest("/auth", auth::create_routes(state.clone()))
                .nest("/status", status::create_routes(state)),
        )
        .merge(swagger::swagger_routes())
        .layer(Config::cors())
        .layer(DefaultBodyLimit::max(10_485_760))
        .layer(GovernorLayer::new(global_governor_conf))
}
