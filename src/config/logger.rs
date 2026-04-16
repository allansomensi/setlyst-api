use super::Config;
use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    fmt::{self},
    layer::SubscriberExt,
};

impl Config {
    pub fn logger_init() -> WorkerGuard {
        let rust_log_file = EnvFilter::from_env("RUST_LOG_FILE");
        let rust_log_console = EnvFilter::from_env("RUST_LOG_CONSOLE");

        let file_appender = rolling::daily("logs", "api.log");
        let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = fmt::Layer::new()
            .with_writer(non_blocking_appender)
            .with_file(true)
            .with_ansi(false)
            .with_line_number(true)
            .with_target(false)
            .with_filter(rust_log_file);

        let console_layer = fmt::Layer::new()
            .pretty()
            .with_file(false)
            .with_ansi(true)
            .with_line_number(false)
            .with_target(false)
            .with_filter(rust_log_console);

        let subscriber = Registry::default().with(console_layer).with(file_layer);

        tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");

        guard
    }
}
