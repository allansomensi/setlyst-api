//! Validation of image links (avatars, band logos).
//!
//! Images are not uploaded: users paste a link to an image hosted
//! elsewhere, which the web loads through its own proxy. The link is
//! therefore restricted to what that proxy can fetch safely and what
//! can't be abused to probe internal services:
//!
//! - `https` only, at most 500 characters;
//! - no credentials (`user:pass@host`);
//! - a public DNS name: no IP literals, no `localhost`, `.local`,
//!   `.internal` or `.localhost` hosts, no single-label hosts;
//! - not an SVG (scriptable) by extension;
//! - not on the list of blocked adult sites.
//!
//! Content checks (keywords, image classification) run afterwards and only
//! raise moderation flags; see `moderation::image`.

use crate::{
    errors::api_error::{ApiError, codes},
    moderation::image::is_blocked_host,
};
use axum::http::StatusCode;
use reqwest::Url;

pub const MAX_IMAGE_URL_LENGTH: usize = 500;

/// Public wildcard-DNS services that turn any name into the IP address
/// embedded in it.
const WILDCARD_DNS_SUFFIXES: &[&str] = &[
    ".nip.io",
    ".sslip.io",
    ".xip.io",
    ".localtest.me",
    ".traefik.me",
    ".lvh.me",
    ".vcap.me",
    ".lacolhost.com",
    ".127-0-0-1.org",
];

fn invalid(message: &str) -> ApiError {
    ApiError::rule(
        StatusCode::BAD_REQUEST,
        codes::INVALID_IMAGE_URL,
        format!("Invalid image link: {message}"),
    )
}

/// Validates `raw` and returns it trimmed. Fails with `INVALID_IMAGE_URL`.
pub fn validate_image_url(raw: &str) -> Result<String, ApiError> {
    let value = raw.trim();
    if value.chars().count() > MAX_IMAGE_URL_LENGTH {
        return Err(invalid("it must be at most 500 characters."));
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid("it contains spaces or control characters."));
    }
    let url = Url::parse(value).map_err(|_| invalid("it is not a valid URL."))?;

    if url.scheme() != "https" {
        return Err(invalid("only https links are accepted."));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("links with credentials are not accepted."));
    }
    if url.host_str().is_none() {
        return Err(invalid("the link has no host."));
    }
    // `domain()` is `None` for IPv4/IPv6 literals.
    let host = match url.domain() {
        Some(host) => host.trim_end_matches('.').to_ascii_lowercase(),
        None => return Err(invalid("IP addresses are not accepted.")),
    };
    if host == "localhost"
        || !host.contains('.')
        || [".local", ".internal", ".localhost", ".lan", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(suffix))
    {
        return Err(invalid("the host is not public."));
    }
    // Wildcard DNS services resolve any name to the address written in
    // it (`10-0-0-5.sslip.io`, `127.0.0.1.nip.io`): an IP literal in
    // disguise, kept out for the same reason literals are.
    if WILDCARD_DNS_SUFFIXES
        .iter()
        .any(|suffix| host == suffix[1..] || host.ends_with(suffix))
    {
        return Err(invalid("the host is not public."));
    }
    // Only the default https port: the image proxy fetches nothing else,
    // and a port is how a link would probe a service rather than a CDN.
    if url.port().is_some() {
        return Err(invalid("links with a port are not accepted."));
    }
    if url.path().to_ascii_lowercase().ends_with(".svg") {
        return Err(invalid("SVG images are not accepted."));
    }
    if is_blocked_host(&host) {
        return Err(invalid("this site is not allowed."));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_https_images() {
        for url in [
            "https://i.imgur.com/abc123.png",
            "https://example.com/photos/me.jpg?size=200",
            " https://cdn.example.org/a.webp ",
            // The default port is dropped by the parser, not a port.
            "https://example.com:443/a.png",
        ] {
            assert!(validate_image_url(url).is_ok(), "{url}");
        }
        assert_eq!(
            validate_image_url(" https://a.example.com/x.png ").unwrap(),
            "https://a.example.com/x.png"
        );
    }

    #[test]
    fn rejects_unsafe_links() {
        for url in [
            "http://example.com/a.png",
            "https://user:pass@example.com/a.png",
            "https://127.0.0.1/a.png",
            "https://[::1]/a.png",
            "https://localhost/a.png",
            "https://printer.local/a.png",
            "https://metadata.internal/a.png",
            "https://intranet/a.png",
            "https://example.com/logo.SVG",
            "https://www.pornhub.com/a.jpg",
            "https://example.com:8443/a.png",
            "https://127.0.0.1.nip.io/a.png",
            "https://10-0-0-5.sslip.io/a.png",
            "https://nip.io/a.png",
            "javascript:alert(1)",
            "data:image/png;base64,AAAA",
            "https://example.com/a b.png",
            "not a url",
        ] {
            let err = validate_image_url(url).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_IMAGE_URL, "{url}");
        }
        let long = format!("https://example.com/{}.png", "a".repeat(500));
        assert!(validate_image_url(&long).is_err());
    }
}
