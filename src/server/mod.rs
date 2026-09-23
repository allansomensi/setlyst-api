use crate::{
    config::Config,
    database::{
        AppState,
        connection::{create_pool, run_migrations},
    },
    errors::api_error::ApiError,
    routes,
};
use std::net::SocketAddr;
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

    let config = Config::get();

    if config.run_migrations {
        match run_migrations(&pool).await {
            Ok(()) => info!("✅ Database migrations are up to date"),
            Err(e) => {
                error!("❌ Failed to apply database migrations: {e}");
                std::process::exit(1);
            }
        }
    }

    let missing = crate::export::pdf::missing_fonts();
    if missing.is_empty() {
        info!(
            "✅ PDF fonts found in {}",
            crate::export::pdf::fonts_dir().display()
        );
    } else {
        // Not fatal: everything but PDF export still works.
        error!(
            "❌ PDF export will fail: font files missing ({}). Set ASSETS_DIR to the directory that contains `fonts/` (the repository's `assets`).",
            missing
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let state = AppState::new(pool);
    crate::jobs::spawn_all(state.clone());
    let app = routes::create_routes(state);

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
