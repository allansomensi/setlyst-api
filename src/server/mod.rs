use crate::{
    database::{
        AppState,
        connection::create_pool,
        repositories::{
            artist_repository::ArtistRepositoryImpl, user_repository::UserRepositoryImpl,
        },
    },
    errors::api_error::ApiError,
    routes,
};
use std::sync::Arc;
use tracing::{error, info};

pub async fn run() -> Result<(), ApiError> {
    let pool = match create_pool().await {
        Ok(pool) => {
            info!("✅ Connected to the database");
            pool
        }
        Err(e) => {
            error!("❌ Error connecting to the database: {e}");
            std::process::exit(1);
        }
    };

    let user_repo = Arc::new(UserRepositoryImpl::new(pool.clone()));
    let artist_repo = Arc::new(ArtistRepositoryImpl::new(pool.clone()));

    let state = AppState {
        db: pool.clone(),
        user_repo,
        artist_repo,
    };

    let app = routes::create_routes(state);

    let addr = std::env::var("HOST")?;
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => {
            info!("✅ Server started at: {}", &addr);
            listener
        }
        Err(e) => {
            error!("❌ Error starting the server: {e}");
            std::process::exit(1)
        }
    };

    axum::serve(listener, app)
        .await
        .expect("Error starting the server");
    Ok(())
}
