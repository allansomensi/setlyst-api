use super::Config;
use tower_http::cors::{Any, CorsLayer};

impl Config {
    pub fn cors() -> CorsLayer {
        let config = Self::get();

        let origins = config.cors_allowed_origins.clone();

        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    }
}
