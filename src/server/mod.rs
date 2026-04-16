use crate::{
    config::Config,
    database::{
        AppState,
        connection::create_pool,
        repositories::{
            artist_repository::ArtistRepositoryImpl, setlist_repository::SetlistRepositoryImpl,
            song_repository::SongRepositoryImpl, user_repository::UserRepositoryImpl,
        },
    },
    errors::api_error::ApiError,
    routes,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::signal;
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
    let song_repo = Arc::new(SongRepositoryImpl::new(pool.clone()));
    let setlist_repo = Arc::new(SetlistRepositoryImpl::new(pool.clone()));

    let state = AppState {
        db: pool.clone(),
        user_repo,
        artist_repo,
        song_repo,
        setlist_repo,
    };

    let app = routes::create_routes(state);

    let config = Config::get();

    let listener = match tokio::net::TcpListener::bind(&config.host).await {
        Ok(listener) => {
            info!("✅ Server started at: {}", &config.host);
            listener
        }
        Err(e) => {
            error!("❌ Error starting the server: {e}");
            std::process::exit(1)
        }
    };

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("Error starting the server");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Shutting down gracefully (Ctrl+C)...");
        },
        _ = terminate => {
            info!("Shutting down gracefully (SIGTERM)...");
        },
    }
}
