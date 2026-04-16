use clap::Parser;
use setlyst_api::{
    config,
    database::{
        AppState,
        connection::create_pool,
        repositories::{
            artist_repository::ArtistRepositoryImpl, setlist_repository::SetlistRepositoryImpl,
            song_repository::SongRepositoryImpl, user_repository::UserRepositoryImpl,
        },
    },
    models::user::{CreateUserPayload, Role, Status},
};
use std::sync::Arc;
use tracing::{error, info};
use validator::Validate;

const DEFAULT_USERNAME: &str = "admin";
const DEFAULT_PASSWORD: &str = "root@toor";

#[derive(clap::Parser, Debug)]
#[command(version, about)]
pub struct Args {
    #[arg(short, long, default_value = DEFAULT_USERNAME)]
    username: String,

    #[arg(short, long, default_value = DEFAULT_PASSWORD)]
    password: String,
}

#[tokio::main]
async fn main() {
    let _guard = match config::Config::init() {
        Ok(guard) => {
            tracing::info!("✅ Configurations loaded!");
            guard
        }
        Err(e) => {
            tracing::error!("❌ Error loading configurations: {e}");
            std::process::exit(1);
        }
    };

    let args = Args::parse();

    let pool = match create_pool().await {
        Ok(pool) => pool,
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

    let user = CreateUserPayload {
        username: args.username,
        password: args.password,
        role: Some(Role::Admin),
        status: Some(Status::default()),
        email: None,
        first_name: None,
        last_name: None,
    };

    user.validate().expect("❌ Validation error");

    state
        .user_repo
        .is_unique(&user.username, None)
        .await
        .expect("❌ Username already exists!");

    match state.user_repo.create(&user).await {
        Ok(new_user) => {
            info!("✅ Superuser created! ID: {}", &new_user.id);
        }
        Err(e) => {
            error!(
                "❌ Error creating superuser with username {}: {e}",
                user.username
            );
        }
    }
}
