use super::Config;
use axum::http::{
    HeaderName,
    header::{ACCEPT, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE, RETRY_AFTER},
    method::Method,
};
use std::time::Duration;
use tower_http::cors::CorsLayer;

impl Config {
    pub fn cors() -> CorsLayer {
        let config = Self::get();

        let origins = config.cors_allowed_origins.clone();

        CorsLayer::new()
            .allow_origin(origins)
            // PUT is used by the band permission matrix; it was missing,
            // so browsers calling it cross-origin got a CORS failure.
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers([
                CONTENT_TYPE,
                AUTHORIZATION,
                ACCEPT,
                HeaderName::from_static("x-app-locale"),
            ])
            // Without this, `fetch` can't read the file name of a PDF or
            // backup download from a cross-origin response, and every
            // export fell back to a generic name.
            .expose_headers([CONTENT_DISPOSITION, RETRY_AFTER])
            .max_age(Duration::from_secs(60 * 60))
    }
}
