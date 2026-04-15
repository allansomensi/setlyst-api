pub mod artist;
pub mod auth;
pub mod migrations;
pub mod setlist;
pub mod song;
pub mod status;
pub mod swagger;
pub mod user;

use crate::{config::Config, database::AppState, middlewares::authentication::authenticate};
use axum::{Router, middleware};

pub fn create_routes(state: AppState) -> Router {
    Router::new()
        .nest(
            "/api/v1",
            Router::new()
                .nest("/users", user::create_routes(state.clone()))
                .nest("/artists", artist::create_routes(state.clone()))
                .nest("/songs", song::create_routes(state.clone()))
                .nest("/setlists", setlist::create_routes(state.clone()))
                .layer(middleware::from_fn_with_state(state.clone(), authenticate))
                .nest("/auth", auth::create_routes(state.clone()))
                .nest("/status", status::create_routes(state.clone()))
                .nest("/migrations", migrations::create_routes(state)),
        )
        .merge(swagger::swagger_routes())
        .layer(Config::cors())
}
