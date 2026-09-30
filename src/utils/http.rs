//! Outbound HTTP clients.
//!
//! `reqwest` is built without a TLS provider of its own (`rustls-no-provider`):
//! the `ring` provider `lettre` already brings in serves it too, which keeps
//! a second TLS stack (and a C build) out of the binary. That provider has
//! to be installed as the process default before the first client exists,
//! which [`client_builder`] guarantees.

use std::sync::Once;

/// Installs `ring` as the process-wide TLS provider, once.
pub fn ensure_tls_provider() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        // Another provider may already have been installed by an earlier
        // caller; either way one is in place afterwards.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A `reqwest` client builder with the TLS provider in place.
pub fn client_builder() -> reqwest::ClientBuilder {
    ensure_tls_provider();
    reqwest::Client::builder()
}

/// A `reqwest` client with the defaults and the TLS provider in place.
pub fn client() -> reqwest::Client {
    client_builder().build().unwrap_or_default_with_provider()
}

trait BuildOrDefault {
    fn unwrap_or_default_with_provider(self) -> reqwest::Client;
}

impl BuildOrDefault for reqwest::Result<reqwest::Client> {
    fn unwrap_or_default_with_provider(self) -> reqwest::Client {
        match self {
            Ok(client) => client,
            Err(_) => reqwest::Client::new(),
        }
    }
}
