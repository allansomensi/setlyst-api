/// Wraps a rate limiter configuration so its per-client state is pruned
/// every minute: without it, the map keyed by client address only ever
/// grows. (A macro, so the limiter's generic types never need naming.)
macro_rules! pruned {
    ($config:expr) => {{
        let config = std::sync::Arc::new($config);
        let limiter = config.limiter().clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
                loop {
                    tick.tick().await;
                    limiter.retain_recent();
                }
            });
        }
        config
    }};
}

/// A per-client rate-limit layer, keyed by [`ClientIpKeyExtractor`] (so a
/// whole IPv6 /64 shares one bucket): `$burst` requests at once, then one
/// more every `$period` (a [`std::time::Duration`]). Pruned like
/// [`pruned!`]. Stack two for a short and a long window, e.g. sign-up at
/// 5 per hour and 20 per day (see [`governor_presets`]):
///
/// ```ignore
/// .route("/register", post(auth::register))
/// .layer(client_governor!(governor_presets::REGISTER_HOURLY))
/// .layer(client_governor!(governor_presets::REGISTER_DAILY))
/// ```
///
/// [`ClientIpKeyExtractor`]: crate::middlewares::client_ip::ClientIpKeyExtractor
macro_rules! client_governor {
    ($preset:expr) => {{
        let (period, burst): (std::time::Duration, u32) = $preset;
        client_governor!(period, burst)
    }};
    ($period:expr, $burst:expr) => {{
        let config = tower_governor::governor::GovernorConfigBuilder::default()
            .period($period)
            .burst_size($burst)
            .key_extractor($crate::middlewares::client_ip::ClientIpKeyExtractor)
            .finish()
            .expect("valid governor configuration");
        tower_governor::GovernorLayer::new(pruned!(config))
    }};
}

/// `(period, burst)` pairs for [`client_governor!`] shared across route
/// modules.
pub mod governor_presets {
    use std::time::Duration;

    /// Self-registration: 5 per hour per client (/64 for IPv6)...
    pub const REGISTER_HOURLY: (Duration, u32) = (Duration::from_secs(12 * 60), 5);
    /// ...and 20 per day. Each sign-up creates an account and e-mails a
    /// code to an arbitrary address, so it is limited far below sign-in.
    pub const REGISTER_DAILY: (Duration, u32) = (Duration::from_secs(72 * 60), 20);
    /// `POST /users/me/reauth/code`: 5 at once, then one every 12 s per
    /// client, on top of the per-account limits (1 a minute, 5 an hour)
    /// enforced by the code issuer.
    pub const REAUTH_CODE: (Duration, u32) = (Duration::from_secs(12), 5);
    /// Linking/unlinking a sign-in provider: 5 at once, then one a minute
    /// per client.
    pub const IDENTITY_LINK: (Duration, u32) = (Duration::from_secs(60), 5);
    /// Anonymous reads of a shared setlist or gig (`/public/...`): 30 at
    /// once, then one every 2 s per client. Each answer carries a whole
    /// running order with lyrics, so it is the most expensive anonymous
    /// read the API has.
    pub const PUBLIC_SHARE_READ: (Duration, u32) = (Duration::from_secs(2), 30);
}

pub mod admin;
pub mod announcement;
pub mod artist;
pub mod auth;
pub mod backup;
pub mod band;
pub mod billing;
pub mod gig;
pub mod health;
pub mod metrics;
pub mod notification;
pub mod pin;
pub mod public;
pub mod setlist;
pub mod song;
pub mod status;
pub mod swagger;
pub mod tour;
pub mod trash;
pub mod user;

use crate::{
    config::Config,
    database::AppState,
    middlewares::{authentication::authenticate, client_ip::ClientIpKeyExtractor},
};
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath},
    http::{HeaderName, HeaderValue, Request, Response, StatusCode, header},
    middleware,
};
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::{
        CompressionLayer, CompressionLevel,
        predicate::{DefaultPredicate, NotForContentType, Predicate},
    },
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::Span;

/// Largest accepted request body, for everything that doesn't declare its
/// own limit. Every JSON payload of the API fits comfortably (the biggest
/// ordinary one, a song with 50 000 characters of lyrics, has its own
/// [`MAX_SONG_BODY_BYTES`]); anything bigger only costs parsing time.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Songs (create/update and the ChordPro import): lyrics are up to 50 000
/// characters, which a client escaping non-ASCII as `\uXXXX` sends as
/// six bytes each.
pub const MAX_SONG_BODY_BYTES: usize = 1024 * 1024;
/// A full backup import, the only large upload.
pub const MAX_IMPORT_BODY_BYTES: usize = 10 * 1024 * 1024;
/// How long a request may take to produce its response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The authenticated API (everything that requires a bearer token).
fn protected_routes(state: AppState) -> Router {
    Router::new()
        .nest("/users", user::create_routes(state.clone()))
        .nest("/artists", artist::create_routes(state.clone()))
        .nest("/songs", song::create_routes(state.clone()))
        .nest("/setlists", setlist::create_routes(state.clone()))
        .nest("/gigs", gig::create_routes(state.clone()))
        .nest("/bands", band::create_routes(state.clone()))
        .nest("/invites", band::create_invite_routes(state.clone()))
        .nest("/notifications", notification::create_routes(state.clone()))
        .nest("/metrics", metrics::create_routes(state.clone()))
        .nest("/backup", backup::create_routes(state.clone()))
        .nest("/admin", admin::create_routes(state.clone()))
        .nest("/billing", billing::create_routes(state.clone()))
        .nest("/announcements", announcement::create_routes(state.clone()))
        .nest("/tours", tour::create_routes(state.clone()))
        .nest("/trash", trash::create_routes(state.clone()))
        .nest("/users/me/pins", pin::create_routes(state.clone()))
        // Authenticated answers are per-user: never let a browser, proxy
        // or service worker keep a copy.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(middleware::from_fn_with_state(state, authenticate))
}

/// The API routes without the global rate limiter — what integration
/// tests drive directly (the IP-keyed governor needs real peer addresses).
/// Carries the request body limit, so tests see the same limits as
/// production.
pub fn api_router(state: AppState) -> Router {
    Router::new()
        .nest(
            "/api/v1",
            protected_routes(state.clone())
                .nest("/auth", auth::create_routes(state.clone()))
                .nest("/status", status::create_routes(state.clone()))
                .nest("/health", health::create_routes())
                .nest(
                    "/public/setlists",
                    setlist::create_public_routes(state.clone()),
                )
                .nest("/public/gigs", gig::create_public_routes(state.clone()))
                .nest("/webhooks", billing::create_webhook_routes(state.clone()))
                .merge(public::create_routes(state)),
        )
        // Routes that need more declare their own `DefaultBodyLimit`, which
        // runs later and wins.
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// The route an access-log line names: the matched pattern (never the
/// concrete path, which can carry share tokens or invite codes).
fn access_log_span(request: &Request<Body>) -> Span {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>");
    tracing::info_span!("request", method = %request.method(), route = %route)
}

fn access_log_line(response: &Response<Body>, latency: Duration, _: &Span) {
    tracing::info!(
        status = response.status().as_u16(),
        latency_ms = latency.as_millis() as u64,
        "request served"
    );
}

pub fn create_routes(state: AppState) -> Router {
    let global_governor_conf = GovernorConfigBuilder::default()
        .per_millisecond(25)
        .burst_size(300)
        .key_extractor(ClientIpKeyExtractor)
        .finish()
        .expect("valid governor configuration");

    let mut router = api_router(state);
    if Config::get().enable_swagger {
        router = router.merge(swagger::swagger_routes());
    }

    // Brotli at its default quality (11) costs seconds of CPU per megabyte
    // on the async runtime; quality 4 compresses JSON almost as well for a
    // fraction of it. PDFs are already compressed, and binary downloads
    // gain nothing.
    let compression = CompressionLayer::new()
        .quality(CompressionLevel::Precise(4))
        .compress_when(
            DefaultPredicate::new()
                .and(NotForContentType::const_new("application/pdf"))
                .and(NotForContentType::const_new("application/octet-stream")),
        );

    // Access log: method, matched route, status and latency. No query
    // string, no headers. Applied with `Router::layer`, i.e. after routing,
    // so the matched route is known.
    let access_log = TraceLayer::new_for_http()
        .make_span_with(access_log_span)
        .on_request(())
        .on_response(access_log_line)
        .on_body_chunk(())
        .on_eos(())
        .on_failure(());

    router
        .layer(access_log)
        .layer(compression)
        // Outside compression, so the time spent producing the (possibly
        // compressed) answer counts. 503: a timeout is the server being
        // overloaded, not the client being slow (browsers may silently
        // resend a request answered with 408).
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            REQUEST_TIMEOUT,
        ))
        // Conservative security headers. The API only serves JSON, PDFs and
        // the Swagger UI, none of which should ever be sniffed or framed.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("cross-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ))
        .layer(GovernorLayer::new(pruned!(global_governor_conf)))
        // A panicking handler answers a plain 500 (with the CORS headers
        // added below) instead of resetting the connection.
        .layer(CatchPanicLayer::new())
        // Outermost, so every answer carries the CORS headers — including a
        // 429 from the rate limiter or a 503 from the timeout, which a
        // browser would otherwise report as an opaque CORS failure.
        .layer(Config::cors())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_governor_presets_build() {
        let _: Router = Router::new()
            .route("/", axum::routing::post(|| async {}))
            .layer(client_governor!(governor_presets::REGISTER_HOURLY))
            .layer(client_governor!(governor_presets::REGISTER_DAILY))
            .layer(client_governor!(Duration::from_secs(60), 3));
    }
}
