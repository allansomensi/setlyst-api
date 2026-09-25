use crate::errors::config_error::ConfigError;
use axum::http::HeaderValue;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ipnet::IpNet;
use std::{path::PathBuf, sync::OnceLock};
use tracing::warn;
use tracing_appender::non_blocking::WorkerGuard;

pub mod cors;
pub mod environment;
pub mod logger;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub database_url: String,
    pub postgres_db: String,
    pub database_max_connections: u32,
    /// Apply pending migrations at startup (default `true`).
    pub run_migrations: bool,
    pub jwt_secret: String,
    pub jwt_expiration_time: i64,
    /// Lifetime of read-only impersonation tokens, in seconds.
    pub impersonation_expiration_time: i64,
    pub cors_allowed_origins: Vec<HeaderValue>,
    /// Public web origin used to build links in e-mails
    /// (`https://www.setlyst.com.br`), without a trailing slash.
    pub app_base_url: String,
    /// AES-256-GCM key for secrets stored in the database (TOTP seeds).
    /// `None` = derived from `JWT_SECRET` (see `utils::crypto`), which
    /// [`Config::from_env`] only allows with `ALLOW_DERIVED_DATA_KEY=true`
    /// (local development): rotating the JWT secret would otherwise make
    /// every stored 2FA secret unreadable.
    pub data_encryption_key: Option<[u8; 32]>,
    /// Static files the server reads at runtime (the PDF fonts live in
    /// `<assets_dir>/fonts`). See [`resolve_assets_dir`].
    pub assets_dir: PathBuf,
    /// Proxies whose `X-Forwarded-For` is trusted. Empty = never trust
    /// forwarding headers.
    pub trusted_proxies: Vec<IpNet>,
    /// Shared secret the web server sends with the real client IP.
    pub internal_api_secret: Option<String>,
    /// OAuth client IDs accepted as the audience of Google ID tokens.
    /// Empty = Google sign-in disabled.
    pub google_client_ids: Vec<String>,
    /// Outgoing e-mail. `None` = e-mails are rendered and logged only.
    pub smtp: Option<SmtpConfig>,
    /// Card payments. `None` = payments are unavailable (checkout answers
    /// `PAYMENTS_UNAVAILABLE`).
    pub stripe: Option<StripeConfig>,
    pub email_worker_interval_secs: u64,
    /// Google Cloud Vision key for image moderation (optional).
    pub moderation_vision_api_key: Option<String>,
    pub trash_retention_days: i64,
    /// Serve the Swagger UI and the OpenAPI document.
    pub enable_swagger: bool,
    /// Public origin of this API (`https://api.setlyst.com.br`), without a
    /// trailing slash. Used for the one-click `List-Unsubscribe` link in
    /// e-mails; `None` = only the web unsubscribe page is advertised.
    pub api_public_url: Option<String>,
    /// Non-security e-mails sent per hour, process-wide, before the worker
    /// holds the rest back (codes and security notices are never held).
    pub email_hourly_cap: i64,
    /// Release builds refuse to start without `TRUSTED_PROXIES` or
    /// `INTERNAL_API_SECRET` (behind a proxy every client would share one
    /// rate-limit bucket) unless this is set (`ALLOW_DIRECT_CLIENTS`).
    pub allow_direct_clients: bool,
    /// Accept test-mode payment keys in a release build without logging an
    /// error (`ALLOW_TEST_PAYMENTS`, for staging).
    pub allow_test_payments: bool,
    /// Don't set the per-session timeouts on new database connections
    /// (`DATABASE_SKIP_SESSION_SETTINGS`), for transaction-mode poolers
    /// that reject or leak session state; set them on the role instead.
    pub database_skip_session_settings: bool,
    /// Image classifications (Google Vision calls) allowed per UTC day;
    /// past it, new images are queued for manual review instead.
    pub vision_daily_budget: i64,
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS (usually port 587).
    StartTls,
    /// Implicit TLS from the first byte (usually port 465).
    Tls,
    /// No encryption. Only for local development relays.
    None,
}

#[derive(Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls: SmtpTls,
    /// `Setlyst <no-reply@setlyst.com.br>`.
    pub from: String,
    pub reply_to: Option<String>,
}

// Hand-written so the SMTP password never ends up in a log line.
impl std::fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("tls", &self.tls)
            .field("from", &self.from)
            .field("reply_to", &self.reply_to)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct StripeConfig {
    /// Secret (`sk_...`) or restricted (`rk_...`) API key.
    pub secret_key: String,
    /// Signing secret of the webhook endpoint (`whsec_...`).
    pub webhook_secret: String,
    /// `https://api.stripe.com`; overridable for a local mock server.
    pub api_base: String,
}

impl StripeConfig {
    /// `true` for test-mode keys.
    pub fn is_test_mode(&self) -> bool {
        self.secret_key.contains("_test_")
    }

    /// `true` for live-mode keys (real money).
    pub fn is_live_mode(&self) -> bool {
        self.secret_key.starts_with("sk_live_") || self.secret_key.starts_with("rk_live_")
    }
}

/// The only API base a live payment key may be sent to.
pub const STRIPE_OFFICIAL_API_BASE: &str = "https://api.stripe.com";

/// Guard rails for real money: a live key only ever talks to Stripe itself
/// (never to a mock or a proxy someone left configured), and the web
/// origin the checkout returns to must be `https`.
pub fn check_live_payments(
    stripe: Option<&StripeConfig>,
    app_base_url: &str,
) -> Result<(), ConfigError> {
    let Some(stripe) = stripe.filter(|s| s.is_live_mode()) else {
        return Ok(());
    };
    if stripe.api_base != STRIPE_OFFICIAL_API_BASE {
        return Err(ConfigError::Invalid(format!(
            "STRIPE_API_BASE must be {STRIPE_OFFICIAL_API_BASE} with a live STRIPE_SECRET_KEY"
        )));
    }
    if !app_base_url.starts_with("https://") {
        return Err(ConfigError::Invalid(
            "APP_BASE_URL must be https:// with a live STRIPE_SECRET_KEY".into(),
        ));
    }
    Ok(())
}

// Hand-written so the keys never end up in a log line.
impl std::fmt::Debug for StripeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeConfig")
            .field("test_mode", &self.is_test_mode())
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl Default for Config {
    /// Development defaults. Production always goes through
    /// [`Config::from_env`]; this exists so tests and tools can build a
    /// configuration by overriding only what they care about.
    fn default() -> Self {
        Self {
            host: "127.0.0.1:8000".to_string(),
            database_url: String::new(),
            postgres_db: "postgres".to_string(),
            database_max_connections: 10,
            run_migrations: true,
            jwt_secret: String::new(),
            jwt_expiration_time: 86_400,
            impersonation_expiration_time: 3_600,
            cors_allowed_origins: Vec::new(),
            app_base_url: "http://localhost:3000".to_string(),
            data_encryption_key: None,
            assets_dir: resolve_assets_dir(None),
            trusted_proxies: Vec::new(),
            internal_api_secret: None,
            google_client_ids: Vec::new(),
            smtp: None,
            stripe: None,
            email_worker_interval_secs: 10,
            moderation_vision_api_key: None,
            trash_retention_days: 30,
            enable_swagger: true,
            api_public_url: None,
            email_hourly_cap: 500,
            allow_direct_clients: false,
            allow_test_payments: false,
            database_skip_session_settings: false,
            vision_daily_budget: 2_000,
        }
    }
}

/// Fragments of the placeholders `.env.example` and setup guides use for
/// secrets: a value carrying one was never replaced.
const PLACEHOLDER_SECRET_FRAGMENTS: &[&str] = &[
    "replace_this",
    "replace-this",
    "replace_me",
    "replace-me",
    "your_secret",
    "your-secret",
    "your_jwt",
    "changeme",
    "change_me",
    "change-me",
    "placeholder",
    "example",
    "secret_key_here",
];

/// A `JWT_SECRET` long enough for the length check but not a secret: a
/// placeholder left in place, or a string with so few distinct characters
/// that it was typed rather than generated (`aaaa…`, `12341234…`). Every
/// derived key (code HMACs, unsubscribe links, the fallback data key)
/// hangs off it.
pub fn jwt_secret_is_weak(secret: &str) -> bool {
    let lower = secret.to_ascii_lowercase();
    if PLACEHOLDER_SECRET_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
    {
        return true;
    }
    let distinct: std::collections::HashSet<char> = secret.chars().collect();
    distinct.len() < 8
}

/// Whether `DATABASE_URL` reaches a remote server without asking for a
/// verified TLS connection: sqlx's default (`sslmode=prefer`) encrypts
/// when the server offers it but never checks the certificate, and
/// silently falls back to plain text. `None` when all is well (a local
/// server, or an explicit `sslmode`).
pub fn database_tls_problem(database_url: &str) -> Option<&'static str> {
    let rest = database_url.split_once("://").map(|(_, r)| r)?;
    let (authority, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host_port = host_port.split('/').next().unwrap_or(host_port);
    let host = host_port
        .strip_prefix('[')
        .and_then(|h| h.split(']').next())
        .unwrap_or_else(|| host_port.split(':').next().unwrap_or(host_port))
        .to_ascii_lowercase();
    let local = host.is_empty()
        || host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.ends_with(".localhost");
    if local {
        return None;
    }
    let mode = query.and_then(|q| {
        q.split('&').find_map(|pair| {
            pair.strip_prefix("sslmode=")
                .or_else(|| pair.strip_prefix("ssl-mode="))
        })
    });
    match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("require") | Some("verify-ca") | Some("verify_ca") | Some("verify-full")
        | Some("verify_full") | Some("verifyca") | Some("verifyfull") => None,
        Some(_) => Some(
            "DATABASE_URL asks for an unencrypted or unverified connection (sslmode); use sslmode=verify-full for a remote server",
        ),
        None => Some(
            "DATABASE_URL has no sslmode: the connection to the remote database is only encrypted if the server happens to offer it, and its certificate is never checked. Add ?sslmode=verify-full (or at least require)",
        ),
    }
}

/// Reads an optional, non-blank variable.
fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_opt(key) {
        Some(v) => !matches!(
            v.to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        ),
        None => default,
    }
}

/// Splits a comma-separated list, dropping blanks.
fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parses `TRUSTED_PROXIES`: CIDRs, or bare addresses (taken as a single
/// host).
pub fn parse_trusted_proxies(raw: &str) -> Result<Vec<IpNet>, ConfigError> {
    split_list(raw)
        .into_iter()
        .map(|entry| {
            entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<std::net::IpAddr>().map(IpNet::from))
                .map_err(|_| ConfigError::Invalid(format!("TRUSTED_PROXIES entry '{entry}'")))
        })
        .collect()
}

/// The data encryption key from `DATA_ENCRYPTION_KEY`. Missing is an
/// error unless `allow_derived` (`ALLOW_DERIVED_DATA_KEY=true`): a key
/// derived from `JWT_SECRET` ties every stored 2FA secret to that secret,
/// so rotating it (e.g. after a leak) would lock every 2FA user out.
fn resolve_data_key(
    raw: Option<&str>,
    allow_derived: bool,
) -> Result<Option<[u8; 32]>, ConfigError> {
    match raw {
        Some(raw) => parse_encryption_key(raw).map(Some),
        None if allow_derived => {
            warn!(
                "DATA_ENCRYPTION_KEY is not set and ALLOW_DERIVED_DATA_KEY=true: deriving the data encryption key from JWT_SECRET. Rotating JWT_SECRET will make stored 2FA secrets unreadable. Never use this in production."
            );
            Ok(None)
        }
        None => Err(ConfigError::MissingDataEncryptionKey),
    }
}

/// Where runtime assets (PDF fonts) are read from: `ASSETS_DIR` when set;
/// else `assets` in the working directory; else, when that doesn't exist
/// (the binary started from another directory), the `assets` directory
/// of the source tree the binary was built from.
pub fn resolve_assets_dir(configured: Option<String>) -> PathBuf {
    if let Some(dir) = configured {
        return PathBuf::from(dir);
    }
    let local = PathBuf::from("assets");
    if local.is_dir() {
        return local;
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"))
}

fn parse_encryption_key(raw: &str) -> Result<[u8; 32], ConfigError> {
    let bytes = BASE64
        .decode(raw.trim())
        .map_err(|_| ConfigError::Invalid("DATA_ENCRYPTION_KEY is not valid base64".into()))?;
    bytes
        .try_into()
        .map_err(|_| ConfigError::Invalid("DATA_ENCRYPTION_KEY must decode to 32 bytes".into()))
}

fn smtp_from_env() -> Result<Option<SmtpConfig>, ConfigError> {
    let Some(host) = env_opt("SMTP_HOST") else {
        return Ok(None);
    };
    let tls = match env_opt("SMTP_TLS")
        .unwrap_or_else(|| "starttls".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "starttls" => SmtpTls::StartTls,
        "tls" | "ssl" => SmtpTls::Tls,
        "none" | "plain" => SmtpTls::None,
        other => return Err(ConfigError::Invalid(format!("SMTP_TLS '{other}'"))),
    };
    let default_port = match tls {
        SmtpTls::StartTls => 587,
        SmtpTls::Tls => 465,
        SmtpTls::None => 25,
    };
    Ok(Some(SmtpConfig {
        host,
        port: env_or("SMTP_PORT", default_port)?,
        username: env_opt("SMTP_USERNAME"),
        password: env_opt("SMTP_PASSWORD"),
        tls,
        from: env_opt("SMTP_FROM")
            .unwrap_or_else(|| "Setlyst <no-reply@setlyst.com.br>".to_string()),
        reply_to: env_opt("SMTP_REPLY_TO"),
    }))
}

/// Payments are on when both Stripe secrets are set; one without the other
/// is a configuration error rather than a silently broken checkout.
pub fn parse_stripe(
    secret_key: Option<String>,
    webhook_secret: Option<String>,
    api_base: Option<String>,
) -> Result<Option<StripeConfig>, ConfigError> {
    match (secret_key, webhook_secret) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(ConfigError::Invalid(
            "STRIPE_WEBHOOK_SECRET is required when STRIPE_SECRET_KEY is set".into(),
        )),
        (None, Some(_)) => Err(ConfigError::Invalid(
            "STRIPE_SECRET_KEY is required when STRIPE_WEBHOOK_SECRET is set".into(),
        )),
        (Some(secret_key), Some(webhook_secret)) => {
            if !(secret_key.starts_with("sk_") || secret_key.starts_with("rk_")) {
                return Err(ConfigError::Invalid(
                    "STRIPE_SECRET_KEY must be a secret (sk_...) or restricted (rk_...) key".into(),
                ));
            }
            if !webhook_secret.starts_with("whsec_") {
                return Err(ConfigError::Invalid(
                    "STRIPE_WEBHOOK_SECRET must be a webhook signing secret (whsec_...)".into(),
                ));
            }
            Ok(Some(StripeConfig {
                secret_key,
                webhook_secret,
                api_base: api_base
                    .unwrap_or_else(|| "https://api.stripe.com".to_string())
                    .trim_end_matches('/')
                    .to_string(),
            }))
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> Result<T, ConfigError>
where
    ConfigError: From<T::Err>,
{
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().parse()?),
        _ => Ok(default),
    }
}

impl Config {
    pub fn init() -> Result<Option<WorkerGuard>, ConfigError> {
        environment::load_environment()?;
        let guard = Self::logger_init();

        let config = Self::from_env()?;
        CONFIG.set(config).expect("Config already initialized");

        Ok(guard)
    }

    /// Builds the configuration from environment variables, validating
    /// everything that would otherwise fail later at runtime.
    pub fn from_env() -> Result<Self, ConfigError> {
        let jwt_secret = std::env::var("JWT_SECRET")?;
        if jwt_secret.len() < 32 || jwt_secret_is_weak(&jwt_secret) {
            return Err(ConfigError::InsecureJwtSecret);
        }

        let cors_raw = std::env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
        let mut cors_allowed_origins = Vec::new();

        for origin in cors_raw
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            cors_allowed_origins.push(origin.parse::<HeaderValue>()?);
        }

        let port = std::env::var("PORT").unwrap_or_else(|_| "8000".to_string());
        let host = format!("0.0.0.0:{port}");

        let run_migrations = std::env::var("RUN_MIGRATIONS")
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        let app_base_url = env_opt("APP_BASE_URL")
            .unwrap_or_else(|| "http://localhost:3000".to_string())
            .trim_end_matches('/')
            .to_string();
        let stripe = parse_stripe(
            env_opt("STRIPE_SECRET_KEY"),
            env_opt("STRIPE_WEBHOOK_SECRET"),
            env_opt("STRIPE_API_BASE"),
        )?;
        check_live_payments(stripe.as_ref(), &app_base_url)?;

        Ok(Config {
            host,
            database_url: std::env::var("DATABASE_URL")?,
            postgres_db: std::env::var("POSTGRES_DB")?,
            database_max_connections: env_or("DATABASE_MAX_CONNECTIONS", 10u32)?,
            run_migrations,
            jwt_secret,
            jwt_expiration_time: env_or("JWT_EXPIRATION_TIME", 86_400i64)?,
            impersonation_expiration_time: env_or("IMPERSONATION_EXPIRATION_TIME", 3_600i64)?,
            cors_allowed_origins,
            app_base_url,
            data_encryption_key: resolve_data_key(
                env_opt("DATA_ENCRYPTION_KEY").as_deref(),
                env_bool("ALLOW_DERIVED_DATA_KEY", false),
            )?,
            assets_dir: resolve_assets_dir(env_opt("ASSETS_DIR")),
            trusted_proxies: parse_trusted_proxies(
                &std::env::var("TRUSTED_PROXIES").unwrap_or_default(),
            )?,
            internal_api_secret: env_opt("INTERNAL_API_SECRET"),
            google_client_ids: split_list(&std::env::var("GOOGLE_CLIENT_IDS").unwrap_or_default()),
            smtp: smtp_from_env()?,
            stripe,
            email_worker_interval_secs: env_or("EMAIL_WORKER_INTERVAL_SECS", 10u64)?.max(1),
            moderation_vision_api_key: env_opt("MODERATION_VISION_API_KEY"),
            trash_retention_days: env_or("TRASH_RETENTION_DAYS", 30i64)?.clamp(1, 3650),
            enable_swagger: env_bool("ENABLE_SWAGGER", cfg!(debug_assertions)),
            api_public_url: env_opt("API_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string()),
            email_hourly_cap: env_or("EMAIL_HOURLY_CAP", 500i64)?.max(0),
            allow_direct_clients: env_bool("ALLOW_DIRECT_CLIENTS", false),
            allow_test_payments: env_bool("ALLOW_TEST_PAYMENTS", false),
            database_skip_session_settings: env_bool("DATABASE_SKIP_SESSION_SETTINGS", false),
            vision_daily_budget: env_or("MODERATION_VISION_DAILY_BUDGET", 2_000i64)?.max(0),
        })
    }

    /// Installs `config` as the process-wide configuration if none is set
    /// yet. Used by integration tests, which build their own config
    /// instead of reading `.env`.
    pub fn init_with(config: Config) {
        let _ = CONFIG.set(config);
    }

    /// The configuration if it has been initialized (unit tests and tools
    /// may run without one).
    pub fn try_get() -> Option<&'static Config> {
        CONFIG.get()
    }

    pub fn get() -> &'static Config {
        CONFIG.get().expect("Config is not initialized.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_and_low_entropy_jwt_secrets_are_refused() {
        assert!(jwt_secret_is_weak(
            "replace_this_value_with_a_secure_32_char_secret"
        ));
        assert!(jwt_secret_is_weak("your_secret_your_secret_your_secret_1"));
        assert!(jwt_secret_is_weak(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(jwt_secret_is_weak("12341234123412341234123412341234"));
        assert!(!jwt_secret_is_weak(
            "kJ8v2Qm9xLp4Zr7Tn3Wb6Yc1Hf5Gd0Se8Ua2Vi4Xo"
        ));
        assert!(!jwt_secret_is_weak(
            "integration-tests-secret-that-is-long-enough"
        ));
    }

    #[test]
    fn remote_databases_must_ask_for_verified_tls() {
        assert!(database_tls_problem("postgres://postgres:postgres@localhost:5432/db").is_none());
        assert!(database_tls_problem("postgres://u:p@127.0.0.1/db").is_none());
        assert!(database_tls_problem("postgres://u:p@[::1]:5432/db").is_none());
        assert!(
            database_tls_problem("postgres://u:p@db.example.com:5432/db?sslmode=verify-full")
                .is_none()
        );
        assert!(database_tls_problem("postgres://u:p@db.example.com/db?sslmode=require").is_none());
        assert!(database_tls_problem("postgres://u:p@db.example.com:5432/db").is_some());
        assert!(database_tls_problem("postgres://u:p@db.example.com/db?sslmode=prefer").is_some());
        assert!(database_tls_problem("postgres://u:p@db.example.com/db?sslmode=disable").is_some());
        assert!(database_tls_problem("postgres://u:p@[2001:db8::7]/db").is_some());
    }

    #[test]
    fn trusted_proxies_accept_cidrs_and_bare_addresses() {
        let nets = parse_trusted_proxies("127.0.0.1/32, 10.0.0.0/8,::1, 192.168.1.10").unwrap();
        assert_eq!(nets.len(), 4);
        assert!(nets[1].contains(&"10.2.3.4".parse::<std::net::IpAddr>().unwrap()));
        assert!(nets[3].contains(&"192.168.1.10".parse::<std::net::IpAddr>().unwrap()));
        assert!(parse_trusted_proxies("not-an-ip").is_err());
        assert!(parse_trusted_proxies("").unwrap().is_empty());
    }

    #[test]
    fn encryption_key_must_be_32_bytes_of_base64() {
        assert!(parse_encryption_key(&BASE64.encode([7u8; 32])).is_ok());
        assert!(parse_encryption_key(&BASE64.encode([7u8; 16])).is_err());
        assert!(parse_encryption_key("%%%").is_err());
    }

    #[test]
    fn a_data_key_is_required_unless_derivation_is_allowed() {
        let key = BASE64.encode([9u8; 32]);
        assert_eq!(
            resolve_data_key(Some(&key), false).unwrap(),
            Some([9u8; 32])
        );
        assert_eq!(resolve_data_key(Some(&key), true).unwrap(), Some([9u8; 32]));
        assert!(matches!(
            resolve_data_key(None, false),
            Err(ConfigError::MissingDataEncryptionKey)
        ));
        assert_eq!(resolve_data_key(None, true).unwrap(), None);
        // A bad key is never silently replaced by the derived one.
        assert!(resolve_data_key(Some("short"), true).is_err());
        let message = ConfigError::MissingDataEncryptionKey.to_string();
        assert!(message.contains("openssl rand -base64 32"), "{message}");
    }

    #[test]
    fn stripe_needs_both_secrets_with_the_right_prefixes() {
        let s = |v: &str| Some(v.to_string());
        assert!(parse_stripe(None, None, None).unwrap().is_none());
        assert!(parse_stripe(s("sk_test_1"), None, None).is_err());
        assert!(parse_stripe(None, s("whsec_1"), None).is_err());
        assert!(parse_stripe(s("pk_test_1"), s("whsec_1"), None).is_err());
        assert!(parse_stripe(s("sk_test_1"), s("secret"), None).is_err());

        let config = parse_stripe(s("sk_test_1"), s("whsec_1"), None)
            .unwrap()
            .unwrap();
        assert_eq!(config.api_base, "https://api.stripe.com");
        assert!(config.is_test_mode());
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk_test_1") && !debug.contains("whsec_1"));

        let live = parse_stripe(s("rk_live_1"), s("whsec_1"), s("http://localhost:12111/"))
            .unwrap()
            .unwrap();
        assert_eq!(live.api_base, "http://localhost:12111");
        assert!(!live.is_test_mode());
    }

    #[test]
    fn live_payment_keys_only_talk_to_stripe_over_https() {
        let s = |v: &str| Some(v.to_string());
        let live = parse_stripe(s("sk_live_1"), s("whsec_1"), None)
            .unwrap()
            .unwrap();
        assert!(live.is_live_mode() && !live.is_test_mode());
        assert!(check_live_payments(Some(&live), "https://www.setlyst.com.br").is_ok());
        assert!(check_live_payments(Some(&live), "http://www.setlyst.com.br").is_err());

        let mocked = parse_stripe(s("rk_live_1"), s("whsec_1"), s("http://localhost:12111"))
            .unwrap()
            .unwrap();
        assert!(check_live_payments(Some(&mocked), "https://www.setlyst.com.br").is_err());

        // Test keys and no payments at all are never refused here.
        let test = parse_stripe(s("sk_test_1"), s("whsec_1"), s("http://localhost:12111"))
            .unwrap()
            .unwrap();
        assert!(check_live_payments(Some(&test), "http://localhost:3000").is_ok());
        assert!(check_live_payments(None, "http://localhost:3000").is_ok());
    }

    #[test]
    fn assets_dir_prefers_the_configured_value() {
        assert_eq!(
            resolve_assets_dir(Some("/srv/setlyst/assets".into())),
            PathBuf::from("/srv/setlyst/assets")
        );
        // Unit tests run from the crate root, where `assets` exists; either
        // way the result must contain the bundled fonts.
        assert!(
            resolve_assets_dir(None)
                .join("fonts/Inter-Regular.ttf")
                .is_file()
        );
    }
}
