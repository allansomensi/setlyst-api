use super::Config;
use axum::http::{
    header::{AUTHORIZATION, CONTENT_TYPE},
    method::Method,
};
use tower_http::cors::CorsLayer;

impl Config {
    pub fn cors() -> CorsLayer {
        let config = Self::get();

        let origins = config.cors_allowed_origins.clone();

        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([CONTENT_TYPE, AUTHORIZATION])
    }
}
