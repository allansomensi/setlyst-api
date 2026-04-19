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
use std::io::{self, Write};
use std::sync::Arc;
use tracing::{error, info};
use validator::Validate;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Optional username. If not provided, the program will prompt for it.
    #[arg(short, long)]
    username: Option<String>,
}

fn validate_password_strength(password: &str) -> Result<(), &'static str> {
    if password.len() < 8 {
        return Err("Password must be at least 8 characters long.");
    }
    if !password.chars().any(|c| c.is_lowercase()) {
        return Err("Password must contain at least one lowercase letter.");
    }
    if !password.chars().any(|c| c.is_uppercase()) {
        return Err("Password must contain at least one uppercase letter.");
    }
    if !password.chars().any(|c| c.is_numeric()) {
        return Err("Password must contain at least one number.");
    }
    if !password.chars().any(|c| !c.is_alphanumeric()) {
        return Err("Password must contain at least one special character.");
    }
    Ok(())
}

fn prompt_for_username() -> String {
    let mut username = String::new();
    loop {
        username.clear();
        print!("Enter the username for the new admin: ");
        io::stdout().flush().expect("❌ Error displaying prompt");

        io::stdin()
            .read_line(&mut username)
            .expect("❌ Error reading username");

        let trimmed_username = username.trim();

        if !trimmed_username.is_empty() {
            return trimmed_username.to_string();
        } else {
            println!("❌ Username cannot be empty.\n");
        }
    }
}

fn prompt_for_password(username: &str) -> String {
    let mut password = String::new();
    loop {
        password.clear();
        print!("Enter a strong password for user '{username}': ");
        io::stdout().flush().expect("❌ Error displaying prompt");

        io::stdin()
            .read_line(&mut password)
            .expect("❌ Error reading password");

        let trimmed_password = password.trim();

        match validate_password_strength(trimmed_password) {
            Ok(_) => return trimmed_password.to_string(),
            Err(e) => println!("❌ Weak password: {}\n", e),
        }
    }
}

#[tokio::main]
async fn main() {
    let _guard = match config::Config::init() {
        Ok(guard) => guard,
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

    // Use the argument if provided, otherwise prompt the user
    let username = match args.username {
        Some(name) => name,
        None => prompt_for_username(),
    };

    let password = prompt_for_password(&username);

    let user = CreateUserPayload {
        username: username.clone(),
        password,
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
