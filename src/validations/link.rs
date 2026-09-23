//! Validation of reference links on songs and setlists.
//!
//! Musicians attach links to a recording, a backing track or a chart kept
//! on a cloud drive. Only a short list of well-known providers is
//! accepted, so a link can't be used to point band mates (or anyone who
//! opens a shared setlist) at an arbitrary site:
//!
//! - `https` only, at most [`MAX_LINK_URL_LENGTH`] characters, no spaces
//!   or control characters;
//! - no credentials (`user:pass@host`) and no explicit port;
//! - the host is one of the provider domains or a subdomain of one
//!   (`youtube.com`, `m.youtube.com`...). Lookalikes such as
//!   `youtube.com.evil.com` or `evilyoutube.com` don't match, and
//!   internationalized hosts are compared in their punycode form, so
//!   homoglyph tricks (`yоutube.com` with a Cyrillic "о") fail as well;
//! - at most [`MAX_LINKS`] links, deduplicated by URL.
//!
//! Anything else is refused with `INVALID_LINK` (`meta.url`).

use crate::{
    errors::api_error::{ApiError, codes},
    models::link::{Link, LinkInput, LinkProvider, StoredLink},
};
use axum::http::StatusCode;
use reqwest::Url;
use serde_json::json;

pub const MAX_LINK_URL_LENGTH: usize = 500;
pub const MAX_LINK_LABEL_LENGTH: usize = 60;
pub const MAX_LINKS: usize = 5;

/// Provider domains. A host matches when it is the domain itself or one
/// of its subdomains.
const PROVIDER_DOMAINS: &[(&str, LinkProvider)] = &[
    ("youtube.com", LinkProvider::Youtube),
    ("youtu.be", LinkProvider::Youtube),
    ("music.youtube.com", LinkProvider::Youtube),
    ("open.spotify.com", LinkProvider::Spotify),
    ("spotify.link", LinkProvider::Spotify),
    ("drive.google.com", LinkProvider::GoogleDrive),
    ("docs.google.com", LinkProvider::GoogleDrive),
    ("music.apple.com", LinkProvider::AppleMusic),
    ("deezer.com", LinkProvider::Deezer),
    ("deezer.page.link", LinkProvider::Deezer),
    ("soundcloud.com", LinkProvider::Soundcloud),
    ("on.soundcloud.com", LinkProvider::Soundcloud),
    ("dropbox.com", LinkProvider::Dropbox),
    ("onedrive.live.com", LinkProvider::Onedrive),
    ("1drv.ms", LinkProvider::Onedrive),
];

fn invalid(url: &str) -> ApiError {
    let shown: String = url.chars().take(MAX_LINK_URL_LENGTH).collect();
    ApiError::rule_with_meta(
        StatusCode::BAD_REQUEST,
        codes::INVALID_LINK,
        "Only https links to YouTube, Spotify, Google Drive, Apple Music, Deezer, SoundCloud, Dropbox or OneDrive are accepted.",
        json!({ "url": shown }),
    )
}

/// The provider of a (lowercase, dot-trimmed) host, if it is an accepted one.
fn provider_for_host(host: &str) -> Option<LinkProvider> {
    PROVIDER_DOMAINS
        .iter()
        .filter(|(domain, _)| {
            host == *domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
        })
        // The most specific domain wins (`music.youtube.com` over `youtube.com`).
        .max_by_key(|(domain, _)| domain.len())
        .map(|(_, provider)| *provider)
}

/// Parses and checks one URL. Returns the normalized URL (as serialized
/// by the URL parser, so it is always percent-encoded ASCII) and its
/// provider.
pub fn validate_link_url(raw: &str) -> Result<(String, LinkProvider), ApiError> {
    let value = raw.trim();
    if value.is_empty()
        || value.chars().count() > MAX_LINK_URL_LENGTH
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || !value.is_ascii() && value.chars().any(is_invisible)
    {
        return Err(invalid(value));
    }
    let url = Url::parse(value).map_err(|_| invalid(value))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(invalid(value));
    }
    // `domain()` is `None` for IP literals.
    let host = url
        .domain()
        .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        .ok_or_else(|| invalid(value))?;
    let provider = provider_for_host(&host).ok_or_else(|| invalid(value))?;

    let normalized = url.to_string();
    if normalized.len() > MAX_LINK_URL_LENGTH {
        return Err(invalid(value));
    }
    Ok((normalized, provider))
}

/// Zero-width and bidirectional control characters.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// The provider of an already stored URL (`None` if it no longer
/// qualifies, e.g. after the provider list changed).
pub fn provider_of(url: &str) -> Option<LinkProvider> {
    validate_link_url(url).ok().map(|(_, provider)| provider)
}

fn clean_label(label: Option<&str>) -> Option<String> {
    let label: String = label?
        .chars()
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .collect();
    let label = label.trim();
    if label.is_empty() {
        None
    } else {
        Some(label.chars().take(MAX_LINK_LABEL_LENGTH).collect())
    }
}

/// Validates a list of links from a payload: every URL must qualify
/// (`INVALID_LINK` otherwise), labels are trimmed (empty = none),
/// duplicates (same normalized URL) are dropped keeping the first, and at
/// most [`MAX_LINKS`] remain (`VALIDATION_ERROR` when more are sent).
pub fn normalize_links(input: &[LinkInput]) -> Result<Vec<StoredLink>, ApiError> {
    let mut out: Vec<StoredLink> = Vec::with_capacity(input.len());
    for link in input {
        if link
            .label
            .as_deref()
            .is_some_and(|l| l.chars().count() > MAX_LINK_LABEL_LENGTH)
        {
            return Err(too_many_or_long(format!(
                "Link labels must be at most {MAX_LINK_LABEL_LENGTH} characters."
            )));
        }
        let (url, _) = validate_link_url(&link.url)?;
        if out.iter().any(|existing| existing.url == url) {
            continue;
        }
        out.push(StoredLink {
            url,
            label: clean_label(link.label.as_deref()),
        });
    }
    if out.len() > MAX_LINKS {
        return Err(too_many_or_long(format!(
            "At most {MAX_LINKS} links are allowed."
        )));
    }
    Ok(out)
}

fn too_many_or_long(message: String) -> ApiError {
    let mut error = validator::ValidationError::new("invalid_links");
    error.message = Some(std::borrow::Cow::from(message));
    let mut errors = validator::ValidationErrors::new();
    errors.add("links", error);
    ApiError::from(errors)
}

/// Stored links as returned to clients (with their provider).
pub fn to_links(stored: Vec<StoredLink>) -> Vec<Link> {
    stored.into_iter().filter_map(Link::from_stored).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(url: &str) -> LinkInput {
        LinkInput {
            url: url.to_string(),
            label: None,
        }
    }

    #[test]
    fn accepts_every_provider() {
        for (url, provider) in [
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                LinkProvider::Youtube,
            ),
            ("https://youtube.com/watch?v=x", LinkProvider::Youtube),
            ("https://m.youtube.com/watch?v=x", LinkProvider::Youtube),
            ("https://youtu.be/dQw4w9WgXcQ", LinkProvider::Youtube),
            ("https://music.youtube.com/watch?v=x", LinkProvider::Youtube),
            (
                "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
                LinkProvider::Spotify,
            ),
            ("https://spotify.link/abc", LinkProvider::Spotify),
            (
                "https://drive.google.com/file/d/1/view",
                LinkProvider::GoogleDrive,
            ),
            (
                "https://docs.google.com/document/d/1/edit",
                LinkProvider::GoogleDrive,
            ),
            (
                "https://music.apple.com/br/album/x/1",
                LinkProvider::AppleMusic,
            ),
            ("https://www.deezer.com/track/1", LinkProvider::Deezer),
            ("https://deezer.page.link/abc", LinkProvider::Deezer),
            ("https://soundcloud.com/a/b", LinkProvider::Soundcloud),
            ("https://on.soundcloud.com/abc", LinkProvider::Soundcloud),
            ("https://www.dropbox.com/s/abc/x.pdf", LinkProvider::Dropbox),
            ("https://onedrive.live.com/?id=1", LinkProvider::Onedrive),
            ("https://1drv.ms/b/s!abc", LinkProvider::Onedrive),
            ("https://YOUTUBE.com./watch?v=x", LinkProvider::Youtube),
        ] {
            let (_, got) = validate_link_url(url).unwrap_or_else(|e| panic!("{url}: {e:?}"));
            assert_eq!(got, provider, "{url}");
        }
    }

    #[test]
    fn rejects_malicious_or_unknown_links() {
        let long = format!("https://youtube.com/watch?v={}", "a".repeat(500));
        for url in [
            "http://youtube.com/watch?v=x",
            "https://user:pass@youtube.com/watch?v=x",
            "https://youtube.com@evil.com/",
            "https://youtube.com:8443/watch",
            "https://142.250.0.1/watch?v=x",
            "https://[::1]/",
            "https://youtube.com.evil.com/watch",
            "https://evilyoutube.com/watch",
            "https://notspotify.link/x",
            "https://spotify.com/track/1",
            "https://google.com/drive",
            "https://apple.com/music",
            "https://y\u{043E}utube.com/watch",
            "https://you\u{200B}tube.com/",
            "https://youtube.com/\u{202E}moc.live",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "ftp://youtube.com/x",
            "https://youtube.com/a b",
            "https://",
            "youtube.com/watch?v=x",
            "",
            &long,
        ] {
            let err = validate_link_url(url).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_LINK, "{url}");
        }
    }

    #[test]
    fn normalizes_deduplicates_and_bounds() {
        let links = normalize_links(&[
            LinkInput {
                url: " https://youtu.be/abc ".to_string(),
                label: Some("  Studio  ".to_string()),
            },
            input("https://youtu.be/abc"),
            LinkInput {
                url: "https://open.spotify.com/track/1".to_string(),
                label: Some("   ".to_string()),
            },
        ])
        .unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].label.as_deref(), Some("Studio"));
        assert_eq!(links[1].label, None);

        let six: Vec<LinkInput> = (0..6)
            .map(|i| input(&format!("https://youtu.be/{i}")))
            .collect();
        assert_eq!(
            normalize_links(&six).unwrap_err().code(),
            "VALIDATION_ERROR"
        );

        let long_label = LinkInput {
            url: "https://youtu.be/x".to_string(),
            label: Some("x".repeat(61)),
        };
        assert_eq!(
            normalize_links(&[long_label]).unwrap_err().code(),
            "VALIDATION_ERROR"
        );

        let bad = normalize_links(&[input("https://evil.com/")]).unwrap_err();
        assert_eq!(bad.code(), codes::INVALID_LINK);
        assert!(normalize_links(&[]).unwrap().is_empty());
    }

    #[test]
    fn stored_links_get_their_provider_back() {
        let links = to_links(vec![
            StoredLink {
                url: "https://youtu.be/abc".to_string(),
                label: None,
            },
            StoredLink {
                url: "https://evil.com/".to_string(),
                label: None,
            },
        ]);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].provider, LinkProvider::Youtube);
    }
}
