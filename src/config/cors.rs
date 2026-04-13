use super::Config;
use tower_http::cors::{Any, CorsLayer};

impl Config {
    pub fn cors() -> CorsLayer {
        let origins = [
            "http://127.0.0.1:3000"
                .parse()
                .expect("Error parsing cors host"),
            "http://localhost:3000"
                .parse()
                .expect("Error parsing cors host"),
        ];

        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any)
    }
}
