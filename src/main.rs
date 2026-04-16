use setlyst_api::{config, errors, server};

#[tokio::main]
async fn main() -> Result<(), errors::api_error::ApiError> {
    println!("🌟 Setlyst API 🌟");

    let _guard = match config::Config::init() {
        Ok(guard) => guard,
        Err(e) => {
            tracing::error!("❌ Error loading configurations: {e}");
            std::process::exit(1);
        }
    };

    server::run().await
}
