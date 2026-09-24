use crate::{
    controllers::auth, database::AppState, middlewares::client_ip::ClientIpKeyExtractor,
    routes::governor_presets,
};
use axum::{Router, routing::post};
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

/// Routes nested under `/auth`. Everything that can be used to guess
/// credentials or codes is rate limited per client IP (resolved safely,
/// see `middlewares::client_ip`), on top of the per-account lockout.
pub fn create_routes(state: AppState) -> Router {
    // Sign-in and second factor: 2 per second, bursts of 5.
    let credentials = GovernorConfigBuilder::default()
        .per_second(2)
        .burst_size(5)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    // Password recovery: 5 per minute and 20 per hour.
    let recovery_minute = GovernorConfigBuilder::default()
        .period(Duration::from_secs(12))
        .burst_size(5)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");
    let recovery_hour = GovernorConfigBuilder::default()
        .period(Duration::from_secs(180))
        .burst_size(20)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    // Google sign-in: one every 2 seconds, bursts of 10.
    let oauth = GovernorConfigBuilder::default()
        .period(Duration::from_secs(2))
        .burst_size(10)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    let credentials_routes = Router::new()
        .route("/login", post(auth::login))
        .route("/login/2fa", post(auth::login_two_factor))
        .layer(GovernorLayer::new(pruned!(credentials)));
    // Sign-up: 5 per hour and 20 per day per client (/64 for IPv6). Every
    // sign-up creates an account and e-mails an address nobody has proven
    // yet. Its own buckets, so sign-ups don't use up the sign-in allowance.
    let register_routes = Router::new()
        .route("/register", post(auth::register))
        .layer(client_governor!(governor_presets::REGISTER_HOURLY))
        .layer(client_governor!(governor_presets::REGISTER_DAILY));

    let recovery_routes = Router::new()
        .route("/password/forgot", post(auth::forgot_password))
        .route("/password/reset", post(auth::reset_password))
        .layer(GovernorLayer::new(pruned!(recovery_minute)))
        .layer(GovernorLayer::new(pruned!(recovery_hour)));

    let oauth_routes = Router::new()
        .route("/oauth/google", post(auth::google_sign_in))
        .layer(GovernorLayer::new(pruned!(oauth)));

    Router::new()
        .merge(credentials_routes)
        .merge(register_routes)
        .merge(recovery_routes)
        .merge(oauth_routes)
        .route("/verify", post(auth::verify))
        .with_state(state)
}
