use super::Config;
use std::env;
use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    fmt::{self},
    layer::SubscriberExt,
};

/// Console log filter when `RUST_LOG_CONSOLE` is unset.
pub const DEFAULT_CONSOLE_FILTER: &str = "info,sqlx=warn,tower_governor=warn";

impl Config {
    pub fn logger_init() -> Option<WorkerGuard> {
        // Unset, `EnvFilter` would only let errors through: warnings
        // (lockouts, delivery retries, moderation failures) and the access
        // log would silently vanish in production.
        let rust_log_console = match env::var("RUST_LOG_CONSOLE") {
            Ok(filter) if !filter.trim().is_empty() => EnvFilter::new(filter),
            _ => EnvFilter::new(DEFAULT_CONSOLE_FILTER),
        };

        // `pretty` (multi-line, coloured) reads well in a terminal; hosted
        // log viewers (Render) want one plain line per event, so that is
        // the default of release builds. `LOG_FORMAT` overrides either.
        let format = env::var("LOG_FORMAT").unwrap_or_else(|_| {
            if cfg!(debug_assertions) {
                "pretty"
            } else {
                "compact"
            }
            .to_string()
        });
        let console_layer = if format.eq_ignore_ascii_case("pretty") {
            fmt::Layer::new()
                .pretty()
                .with_file(false)
                .with_ansi(true)
                .with_line_number(false)
                .with_target(false)
                .with_filter(rust_log_console)
                .boxed()
        } else {
            fmt::Layer::new()
                .compact()
                .with_ansi(false)
                .with_target(false)
                .with_filter(rust_log_console)
                .boxed()
        };

        let log_to_file = env::var("LOG_TO_FILE").unwrap_or_default() == "true";

        let (file_layer, guard) = if log_to_file {
            let rust_log_file = EnvFilter::from_env("RUST_LOG_FILE");
            let file_appender = rolling::daily("logs", "api.log");
            let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

            let layer = fmt::Layer::new()
                .with_writer(non_blocking_appender)
                .with_file(true)
                .with_ansi(false)
                .with_line_number(true)
                .with_target(false)
                .with_filter(rust_log_file);

            (Some(layer), Some(guard))
        } else {
            (None, None)
        };

        let subscriber = Registry::default().with(console_layer).with(file_layer);

        tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

        guard
    }
}
