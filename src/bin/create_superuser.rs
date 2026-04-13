use clap::Parser;
use setlyst_api::{
    config,
    database::{AppState, connection::create_pool},
    models::user::{CreateUserPayload, Role, Status, User},
    validations::uniqueness::is_user_unique,
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
    config::Config::init().expect("Config error");
    let args = Args::parse();

    let pool = match create_pool().await {
        Ok(pool) => pool,
        Err(e) => {
            error!("❌ Error connecting to the database: {e}");
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState { db: pool.clone() });

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
    is_user_unique(&state, &user.username)
        .await
        .expect("❌ Username already exists!");

    match User::create(&state, &user).await {
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
