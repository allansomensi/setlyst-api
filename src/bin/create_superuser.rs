use clap::Parser;
use setlyst_api::{
    config,
    database::{
        AppState,
        connection::{create_pool, run_migrations},
        repositories::audit_repository::AuditEvent,
    },
    middlewares::authentication::STAFF_TWO_FACTOR_GRACE_MINUTES,
    models::{
        audit::actions,
        user::{CreateUserPayload, Role, Status, UpdateUserPayload},
    },
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

    /// Demotes the admin with this username to a regular user instead of
    /// creating one. Admins can't change each other's role through the
    /// API (a compromised admin could otherwise take over a peer), so
    /// this command, which needs access to the server, is the way to step
    /// an admin down. The last active admin can't be demoted.
    #[arg(long, value_name = "USERNAME")]
    demote: Option<String>,
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
        "breached" => "not appearing in a known data breach",
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

/// Reads a password from the terminal without echoing it (it must never
/// end up in the scrollback or a screen recording).
fn read_secret(label: &str) -> String {
    match rpassword::prompt_password(label) {
        Ok(value) => value,
        Err(e) => {
            eprintln!("❌ Error reading the password: {e}");
            std::process::exit(1);
        }
    }
}

fn prompt_for_password(username: &str) -> String {
    loop {
        let password = read_secret(&format!("Enter a strong password for user '{username}': "));
        let issues = password_issues(&password, Some(username));
        if !issues.is_empty() {
            let missing: Vec<&str> = issues.iter().map(|i| describe_issue(i)).collect();
            println!("❌ Weak password: it needs {}.\n", missing.join(", "));
            continue;
        }
        let confirmation = read_secret("Repeat the password: ");
        if confirmation == password {
            return password;
        }
        println!("❌ The passwords don't match.\n");
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

    if let Some(username) = args.demote {
        demote(&state, &username).await;
        return;
    }

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
        Ok(new_user) => {
            println!("✅ Superuser created! ID: {}", new_user.id);
            println!(
                "⚠️  Staff accounts must enable two-factor authentication: sign in within {STAFF_TWO_FACTOR_GRACE_MINUTES} minutes and turn it on in Settings > Security. Until then, everything but the account settings answers STAFF_TWO_FACTOR_REQUIRED."
            );
            println!(
                "⚠️  The account has no e-mail address: add and verify one in Settings (two-factor setup and password recovery need it)."
            );
        }
        Err(e) => {
            eprintln!("❌ Error creating superuser '{}': {e}", user.username);
            std::process::exit(1);
        }
    }
}

/// Demotes the admin `username` to a regular user (recorded in the audit
/// log, with every session of the account signed out).
async fn demote(state: &AppState, username: &str) {
    let account = match state.user_repo.find_by_username(username).await {
        Ok(Some(account)) if account.role == Role::Admin => account,
        Ok(Some(_)) => {
            eprintln!("❌ '{username}' is not an admin.");
            std::process::exit(1);
        }
        Ok(None) => {
            eprintln!("❌ No account named '{username}'.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("❌ Error looking the account up: {e}");
            std::process::exit(1);
        }
    };
    let payload = UpdateUserPayload {
        role: Some(Role::User),
        ..Default::default()
    };
    if let Err(e) = state.user_repo.update(account.id, &payload, None).await {
        eprintln!("❌ Could not demote '{username}': {e}");
        std::process::exit(1);
    }
    if let Err(e) = state.user_repo.revoke_sessions(account.id).await {
        eprintln!("⚠️  Demoted, but the sessions could not be revoked: {e}");
    }
    AuditEvent::new(actions::USER_ROLE_CHANGED)
        .target("user", account.id, &account.username)
        .meta(serde_json::json!({ "from": Role::Admin, "to": Role::User, "source": "cli" }))
        .record(&*state.audit_repo)
        .await;
    println!("✅ '{username}' is now a regular user; every session was signed out.");
}
