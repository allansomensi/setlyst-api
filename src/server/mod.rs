use crate::{
    config::{Config, SmtpTls, database_tls_problem},
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

/// Settings that are only defaults for local development. A release build
/// refuses to start with a broken e-mail setup (sign-up, password reset and
/// receipts depend on it) and complains loudly about the rest.
fn check_production_config(config: &Config) {
    match crate::email::worker::check_smtp_config(config) {
        Ok(true) => info!("✅ SMTP configured"),
        Ok(false) if cfg!(debug_assertions) => {
            info!("SMTP is not configured: e-mails are printed to the log (development build)")
        }
        Ok(false) => error!(
            "❌ SMTP is not configured: no e-mail (verification, password reset, receipts) will be sent. Set SMTP_HOST and friends."
        ),
        Err(e) if cfg!(debug_assertions) => error!("❌ SMTP configuration is invalid: {e}"),
        Err(e) => {
            error!("❌ SMTP configuration is invalid: {e}");
            std::process::exit(1);
        }
    }
    if cfg!(debug_assertions) {
        return;
    }
    if let Some(problem) = direct_clients_problem(config) {
        error!("❌ {problem}");
        std::process::exit(1);
    }
    if let Some(problem) = insecure_smtp_problem(config) {
        error!("❌ {problem}");
        std::process::exit(1);
    }
    if let Some(problem) = database_tls_problem(&config.database_url) {
        error!("❌ {problem}");
    }
    if let Some(stripe) = &config.stripe
        && stripe.is_test_mode()
        && !config.allow_test_payments
    {
        error!(
            "❌ STRIPE_SECRET_KEY is a test-mode key in a release build: no real payment will be taken. Use a live key, or set ALLOW_TEST_PAYMENTS=true on staging."
        );
    }
    if config.app_base_url.contains("localhost") || config.app_base_url.contains("127.0.0.1") {
        error!(
            "❌ APP_BASE_URL is {}: links in e-mails and Stripe redirects will point to it. Set it to the public web address.",
            config.app_base_url
        );
    }
    if config.enable_swagger {
        info!("⚠️  Swagger UI is enabled (ENABLE_SWAGGER); consider turning it off in production");
    }
}

/// Why a release build must not start with this client-IP setup, if it
/// mustn't: with neither `TRUSTED_PROXIES` nor `INTERNAL_API_SECRET`, an
/// API behind a proxy (Render, Cloudflare, the web server) sees every
/// visitor as the proxy, so all of them share one rate-limit bucket and a
/// single script can lock everyone out of signing in. Serving clients
/// directly is legitimate, but has to be said (`ALLOW_DIRECT_CLIENTS=true`).
pub fn direct_clients_problem(config: &Config) -> Option<&'static str> {
    (config.trusted_proxies.is_empty()
        && config.internal_api_secret.is_none()
        && !config.allow_direct_clients)
        .then_some(
            "Neither TRUSTED_PROXIES nor INTERNAL_API_SECRET is set: behind a proxy every client would share one rate-limit bucket. Set TRUSTED_PROXIES (e.g. 10.0.0.0/8 on Render) and/or INTERNAL_API_SECRET, or ALLOW_DIRECT_CLIENTS=true if clients really connect directly.",
        )
}

/// Why a release build must not start with this SMTP setup, if it
/// mustn't: `SMTP_TLS=none` sends every one-time code, password reset and
/// sign-in notice in plain text across the network. Only a relay on this
/// very machine may be spoken to unencrypted.
pub fn insecure_smtp_problem(config: &Config) -> Option<&'static str> {
    let smtp = config.smtp.as_ref()?;
    if smtp.tls != SmtpTls::None {
        return None;
    }
    let host = smtp
        .host
        .trim_matches(|c| c == '[' || c == ']')
        .to_ascii_lowercase();
    let loopback = host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    (!loopback).then_some(
        "SMTP_TLS=none with a remote SMTP_HOST: one-time codes and password resets would travel unencrypted. Use SMTP_TLS=starttls or tls (none is only for a relay on this machine).",
    )
}

/// How long the startup check of the Stripe key's scopes may take.
const PAYMENT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Checks that the Stripe key can read what the webhook and the finance
/// report need (see [`crate::services::payments::probe_permissions`]).
/// Missing scopes are logged; with a live key, a scope Stripe refuses
/// (403) stops the server, since every paid invoice would then fail to be
/// recorded. Anything else (Stripe unreachable, a timeout) is only logged:
/// the rest of the API must not depend on Stripe being up.
async fn check_payment_permissions(state: &AppState, config: &Config) {
    let Some(stripe) = &config.stripe else {
        return;
    };
    let probe = match tokio::time::timeout(
        PAYMENT_PROBE_TIMEOUT,
        crate::services::payments::probe_permissions(state),
    )
    .await
    {
        Ok(probe) => probe,
        Err(_) => {
            error!("❌ Could not check the Stripe key's permissions in time; continuing");
            return;
        }
    };
    if probe.is_clean() {
        info!("✅ Stripe key permissions checked");
        return;
    }
    if payment_permissions_fatal(stripe.is_live_mode(), &probe) {
        error!(
            "❌ The live STRIPE_SECRET_KEY lacks required permissions ({}). Grant them to the restricted key (see .env.example) and restart.",
            probe.denied.join(", ")
        );
        std::process::exit(1);
    }
    error!(
        denied = %probe.denied.join(", "),
        unchecked = %probe.unchecked.join(", "),
        "❌ Some Stripe key permissions are missing or could not be checked; payments may fail"
    );
}

/// `true` when the server must not start: a live key that Stripe refuses
/// a required read scope.
pub fn payment_permissions_fatal(
    live: bool,
    probe: &crate::services::payments::PermissionProbe,
) -> bool {
    live && !probe.denied.is_empty()
}

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

    check_production_config(config);

    let state = AppState::new(pool);
    check_payment_permissions(&state, config).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SmtpConfig;

    #[test]
    fn a_proxied_setup_must_say_how_to_find_the_client() {
        let bare = Config::default();
        assert!(direct_clients_problem(&bare).is_some());

        let proxied = Config {
            trusted_proxies: crate::config::parse_trusted_proxies("10.0.0.0/8").unwrap(),
            ..Config::default()
        };
        assert!(direct_clients_problem(&proxied).is_none());

        let internal = Config {
            internal_api_secret: Some("a-shared-secret".into()),
            ..Config::default()
        };
        assert!(direct_clients_problem(&internal).is_none());

        let direct = Config {
            allow_direct_clients: true,
            ..Config::default()
        };
        assert!(direct_clients_problem(&direct).is_none());
    }

    #[test]
    fn unencrypted_smtp_is_only_allowed_to_a_local_relay() {
        let smtp = |host: &str, tls: SmtpTls| SmtpConfig {
            host: host.to_string(),
            port: 25,
            username: None,
            password: None,
            tls,
            from: "Setlyst <no-reply@setlyst.test>".to_string(),
            reply_to: None,
        };
        let with = |smtp: SmtpConfig| Config {
            smtp: Some(smtp),
            ..Config::default()
        };
        assert!(insecure_smtp_problem(&Config::default()).is_none());
        assert!(insecure_smtp_problem(&with(smtp("localhost", SmtpTls::None))).is_none());
        assert!(insecure_smtp_problem(&with(smtp("127.0.0.1", SmtpTls::None))).is_none());
        assert!(insecure_smtp_problem(&with(smtp("::1", SmtpTls::None))).is_none());
        assert!(
            insecure_smtp_problem(&with(smtp("smtp.example.com", SmtpTls::StartTls))).is_none()
        );
        assert!(insecure_smtp_problem(&with(smtp("smtp.example.com", SmtpTls::Tls))).is_none());
        assert!(insecure_smtp_problem(&with(smtp("smtp.example.com", SmtpTls::None))).is_some());
        assert!(insecure_smtp_problem(&with(smtp("10.0.0.5", SmtpTls::None))).is_some());
    }

    #[test]
    fn only_a_live_key_refused_a_scope_stops_the_server() {
        use crate::services::payments::PermissionProbe;
        let denied = PermissionProbe {
            denied: vec!["Refunds (read)".into()],
            unchecked: Vec::new(),
        };
        let unreachable = PermissionProbe {
            denied: Vec::new(),
            unchecked: vec!["Invoices (read)".into()],
        };
        assert!(payment_permissions_fatal(true, &denied));
        assert!(!payment_permissions_fatal(false, &denied));
        assert!(!payment_permissions_fatal(true, &unreachable));
        assert!(!payment_permissions_fatal(
            true,
            &PermissionProbe::default()
        ));
        assert!(PermissionProbe::default().is_clean());
        assert!(!unreachable.is_clean());
    }
}
