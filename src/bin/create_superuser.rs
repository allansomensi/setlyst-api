use clap::Parser;
use setlyst_api::{
    config,
    database::{
        AppState,
        connection::{create_pool, run_migrations},
    },
    models::user::{CreateUserPayload, Role, Status},
    validations::{password::password_issues, username::validate_username},
};
use std::io::{self, Write};
use validator::Validate;

#[derive(clap::Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Optional username. If not provided, the program will prompt for it.
    #[arg(short, long)]
    username: Option<String>,
}

/// Human-readable explanations for the password policy's issue codes.
fn describe_issue(issue: &str) -> &'static str {
    match issue {
        "too_short" => "at least 8 characters",
        "too_long" => "at most 128 characters",
        "missing_lowercase" => "a lowercase letter",
        "missing_uppercase" => "an uppercase letter",
        "missing_digit" => "a number",
        "missing_symbol" => "a symbol (e.g. ! @ # -)",
        "contains_username" => "not containing the username",
        "too_common" => "not being a commonly used password",
        _ => "meeting the password policy",
    }
}

fn prompt(label: &str) -> String {
    let mut value = String::new();
    print!("{label}");
    io::stdout().flush().expect("❌ Error displaying prompt");
    io::stdin()
        .read_line(&mut value)
        .expect("❌ Error reading input");
    value.trim().to_string()
}

fn prompt_for_username() -> String {
    loop {
        let username = prompt("Enter the username for the new admin: ");
        match validate_username(&username) {
            Ok(()) => return username,
            Err(e) => println!(
                "❌ {}\n",
                e.message.map(|m| m.to_string()).unwrap_or_default()
            ),
        }
    }
}

fn prompt_for_password(username: &str) -> String {
    loop {
        let password = prompt(&format!("Enter a strong password for user '{username}': "));
        let issues = password_issues(&password, Some(username));
        if issues.is_empty() {
            return password;
        }
        let missing: Vec<&str> = issues.iter().map(|i| describe_issue(i)).collect();
        println!("❌ Weak password — it needs {}.\n", missing.join(", "));
    }
}

#[tokio::main]
async fn main() {
    let _guard = match config::Config::init() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("❌ Error loading configurations: {e}");
            std::process::exit(1);
        }
    };

    let args = Args::parse();

    let pool = match create_pool().await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("❌ Error connecting to the database: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run_migrations(&pool).await {
        eprintln!("❌ Failed to apply database migrations: {e}");
        std::process::exit(1);
    }

    let state = AppState::new(pool);

    let username = match args.username {
        Some(name) => match validate_username(&name) {
            Ok(()) => name,
            Err(e) => {
                eprintln!(
                    "❌ {}",
                    e.message.map(|m| m.to_string()).unwrap_or_default()
                );
                std::process::exit(1);
            }
        },
        None => prompt_for_username(),
    };

    if state.user_repo.is_unique(&username, None).await.is_err() {
        eprintln!("❌ Username '{username}' already exists!");
        std::process::exit(1);
    }

    let password = prompt_for_password(&username);

    let user = CreateUserPayload {
        username: username.clone(),
        password,
        role: Some(Role::Admin),
        status: Some(Status::default()),
        email: None,
        first_name: None,
        last_name: None,
        require_password_change: Some(false),
    };

    if let Err(e) = user.validate() {
        eprintln!("❌ Validation error: {e}");
        std::process::exit(1);
    }

    match state.user_repo.create(&user, None, false).await {
        Ok(new_user) => println!("✅ Superuser created! ID: {}", new_user.id),
        Err(e) => {
            eprintln!("❌ Error creating superuser '{}': {e}", user.username);
            std::process::exit(1);
        }
    }
}
