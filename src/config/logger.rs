use super::Config;
use chrono::{DateTime, FixedOffset, Utc};
use tracing_appender::rolling;
use tracing_subscriber::{
    fmt::{self, format::Writer, time::FormatTime},
    layer::SubscriberExt,
    EnvFilter, Layer, Registry,
};

impl Config {
    pub fn logger_init() {
        struct UtcFormattedTime;

        impl FormatTime for UtcFormattedTime {
            fn format_time(&self, writer: &mut Writer<'_>) -> std::fmt::Result {
                let brasilia_offset = FixedOffset::west_opt(3 * 3600).unwrap();
                let now: DateTime<FixedOffset> = Utc::now().with_timezone(&brasilia_offset);
                let formatted_time = now.format("%d/%m/%Y %H:%M:%S").to_string();
                write!(writer, "{}", formatted_time)
            }
        }

        let rust_log_file = EnvFilter::from_env("RUST_LOG_FILE");
        let rust_log_console = EnvFilter::from_env("RUST_LOG_CONSOLE");

        let file_appender = rolling::daily("logs", "api.log");

        let file_layer = fmt::Layer::new()
            .with_timer(UtcFormattedTime)
            .with_writer(file_appender)
            .with_file(true)
            .with_ansi(false)
            .with_line_number(true)
            .with_target(false)
            .with_filter(rust_log_file);

        let console_layer = fmt::Layer::new()
            .pretty()
            .with_timer(UtcFormattedTime)
            .with_file(false)
            .with_ansi(true)
            .with_line_number(false)
            .with_target(false)
            .with_filter(rust_log_console);

        let subscriber = Registry::default().with(console_layer).with(file_layer);

        tracing::subscriber::set_global_default(subscriber).expect("Failed to set subscriber");
    }
}
