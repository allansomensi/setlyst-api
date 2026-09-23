//! Who is the client? The one answer used for rate limiting and audit.
//!
//! Forwarding headers are trivially forged, so they are only believed
//! when the connection comes from a proxy we trust:
//!
//! 1. `X-Setlyst-Internal: <INTERNAL_API_SECRET>` (compared in constant
//!    time) together with `X-Setlyst-Client-IP: <ip>` → that IP. This is
//!    how the web server, which calls the API on the user's behalf, passes
//!    the real visitor address.
//! 2. The socket peer is in `TRUSTED_PROXIES` → the rightmost address in
//!    `X-Forwarded-For` that is not itself a trusted proxy (each proxy
//!    appends the address it received the request from, so everything
//!    left of the first untrusted hop may have been written by the client).
//! 3. Otherwise → the socket peer.
//!
//! Requests without socket information at all (the router driven
//! in-process, as the integration tests do) have no peer to distrust: the
//! forwarding header is then treated as coming from a trusted hop, and when
//! there is none either, every such request shares the `0.0.0.0` key. The
//! production server always provides the peer address.

use crate::config::Config;
use axum::{extract::ConnectInfo, http::HeaderMap, http::Request};
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower_governor::{GovernorError, key_extractor::KeyExtractor};

pub const INTERNAL_SECRET_HEADER: &str = "x-setlyst-internal";
pub const INTERNAL_CLIENT_IP_HEADER: &str = "x-setlyst-client-ip";

/// The key shared by requests whose origin can't be determined.
pub const UNKNOWN_CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Everything the resolution depends on besides the request itself.
#[derive(Debug, Clone, Copy)]
pub struct ClientIpPolicy<'a> {
    pub trusted_proxies: &'a [IpNet],
    pub internal_secret: Option<&'a str>,
}

impl<'a> ClientIpPolicy<'a> {
    pub fn from_config(config: &'a Config) -> Self {
        Self {
            trusted_proxies: &config.trusted_proxies,
            internal_secret: config.internal_api_secret.as_deref(),
        }
    }

    fn is_trusted(&self, ip: &IpAddr) -> bool {
        self.trusted_proxies.iter().any(|net| net.contains(ip))
    }
}

/// Parses an address as it appears in forwarding headers (optionally
/// bracketed IPv6, optionally with a port).
fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim().trim_matches('"');
    if let Ok(ip) = raw.parse::<IpAddr>() {
        return Some(normalize(ip));
    }
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Some(normalize(addr.ip()));
    }
    raw.strip_prefix('[')
        .and_then(|r| r.split(']').next())
        .and_then(|r| r.parse::<IpAddr>().ok())
        .map(normalize)
}

/// IPv4-mapped IPv6 addresses (`::ffff:1.2.3.4`) count as their IPv4 form,
/// so one client can't get two rate-limit buckets.
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The rightmost `X-Forwarded-For` hop that isn't a trusted proxy.
///
/// Hops that don't parse as an address (`unknown`, obfuscated identifiers,
/// junk a client prepended) are skipped rather than invalidating the whole
/// header: otherwise one bad entry would make every client behind the
/// proxy share the proxy's rate-limit bucket.
fn forwarded_client(headers: &HeaderMap, policy: &ClientIpPolicy) -> Option<IpAddr> {
    let hops: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(parse_ip)
        .collect();
    hops.iter()
        .rev()
        .find(|ip| !policy.is_trusted(ip))
        .or_else(|| hops.first())
        .copied()
}

/// Resolves the client address of a request (see the module docs).
/// `None` only when there is neither a peer nor usable header.
pub fn resolve_client_ip(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    policy: &ClientIpPolicy,
) -> Option<IpAddr> {
    if let (Some(secret), Some(sent)) = (
        policy.internal_secret,
        header(headers, INTERNAL_SECRET_HEADER),
    ) && crate::utils::crypto::constant_time_eq(secret.as_bytes(), sent.trim().as_bytes())
        && let Some(ip) = header(headers, INTERNAL_CLIENT_IP_HEADER).and_then(parse_ip)
    {
        return Some(ip);
    }

    match peer.map(normalize) {
        Some(peer) if policy.is_trusted(&peer) => forwarded_client(headers, policy).or(Some(peer)),
        Some(peer) => Some(peer),
        None => forwarded_client(headers, policy),
    }
}

/// The socket peer recorded by `into_make_service_with_connect_info`.
pub fn peer_of<T>(req: &Request<T>) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// Resolution with the process configuration (or the conservative
/// defaults when none is loaded).
pub fn resolve_with_config(peer: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    match Config::try_get() {
        Some(config) => resolve_client_ip(peer, headers, &ClientIpPolicy::from_config(config)),
        None => resolve_client_ip(
            peer,
            headers,
            &ClientIpPolicy {
                trusted_proxies: &[],
                internal_secret: None,
            },
        ),
    }
}

/// `tower_governor` key extractor built on [`resolve_client_ip`]. Never
/// fails: an unknown origin maps to [`UNKNOWN_CLIENT`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ClientIpKeyExtractor;

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = IpAddr;

    fn name(&self) -> &'static str {
        "client IP"
    }

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        Ok(resolve_with_config(peer_of(req), req.headers()).unwrap_or(UNKNOWN_CLIENT))
    }

    fn key_name(&self, key: &Self::Key) -> Option<String> {
        Some(key.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|n| n.parse().unwrap()).collect()
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.append(*k, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn untrusted_peers_cannot_spoof_forwarding_headers() {
        let trusted = nets(&["10.0.0.0/8"]);
        let policy = ClientIpPolicy {
            trusted_proxies: &trusted,
            internal_secret: None,
        };
        let h = headers(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(
            resolve_client_ip(Some(ip("203.0.113.9")), &h, &policy),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn trusted_peers_yield_the_rightmost_untrusted_hop() {
        let trusted = nets(&["10.0.0.0/8", "127.0.0.1/32"]);
        let policy = ClientIpPolicy {
            trusted_proxies: &trusted,
            internal_secret: None,
        };
        // The client forged "6.6.6.6"; the edge proxy appended the real
        // address, the inner proxy appended the edge.
        let h = headers(&[("x-forwarded-for", "6.6.6.6, 198.51.100.7, 10.1.1.1")]);
        assert_eq!(
            resolve_client_ip(Some(ip("127.0.0.1")), &h, &policy),
            Some(ip("198.51.100.7"))
        );
        // Split across several header lines.
        let h = headers(&[
            ("x-forwarded-for", "6.6.6.6"),
            ("x-forwarded-for", "198.51.100.7"),
        ]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &h, &policy),
            Some(ip("198.51.100.7"))
        );
        // No header: the proxy itself.
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &HeaderMap::new(), &policy),
            Some(ip("10.0.0.2"))
        );
        // Garbage in the header: fall back to the peer.
        let h = headers(&[("x-forwarded-for", "not-an-ip")]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &h, &policy),
            Some(ip("10.0.0.2"))
        );
    }

    #[test]
    fn unparseable_hops_are_skipped_not_fatal() {
        let trusted = nets(&["10.0.0.0/8"]);
        let policy = ClientIpPolicy {
            trusted_proxies: &trusted,
            internal_secret: None,
        };
        // Junk the client prepended doesn't hide the address the edge
        // proxy appended.
        let h = headers(&[("x-forwarded-for", "garbage, 198.51.100.7, 10.1.1.1")]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &h, &policy),
            Some(ip("198.51.100.7"))
        );
        // An `unknown` hop between the client and the trusted proxies is
        // skipped while walking from the right.
        let h = headers(&[
            ("x-forwarded-for", "198.51.100.8"),
            ("x-forwarded-for", "unknown, 10.2.2.2"),
        ]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &h, &policy),
            Some(ip("198.51.100.8"))
        );
        // Only junk and trusted hops: the leftmost parseable one.
        let h = headers(&[("x-forwarded-for", "junk, 10.3.3.3")]);
        assert_eq!(
            resolve_client_ip(Some(ip("10.0.0.2")), &h, &policy),
            Some(ip("10.3.3.3"))
        );
    }

    #[test]
    fn the_internal_header_needs_the_right_secret() {
        let policy = ClientIpPolicy {
            trusted_proxies: &[],
            internal_secret: Some("s3cret-value"),
        };
        let good = headers(&[
            (INTERNAL_SECRET_HEADER, "s3cret-value"),
            (INTERNAL_CLIENT_IP_HEADER, "198.51.100.20"),
        ]);
        assert_eq!(
            resolve_client_ip(Some(ip("203.0.113.1")), &good, &policy),
            Some(ip("198.51.100.20"))
        );

        let bad = headers(&[
            (INTERNAL_SECRET_HEADER, "guess"),
            (INTERNAL_CLIENT_IP_HEADER, "198.51.100.20"),
        ]);
        assert_eq!(
            resolve_client_ip(Some(ip("203.0.113.1")), &bad, &policy),
            Some(ip("203.0.113.1"))
        );

        // Without a configured secret the header is never honoured.
        let none = ClientIpPolicy {
            trusted_proxies: &[],
            internal_secret: None,
        };
        assert_eq!(
            resolve_client_ip(Some(ip("203.0.113.1")), &good, &none),
            Some(ip("203.0.113.1"))
        );
    }

    #[test]
    fn ipv6_and_mapped_addresses_are_handled() {
        let trusted = nets(&["::1/128", "fd00::/8"]);
        let policy = ClientIpPolicy {
            trusted_proxies: &trusted,
            internal_secret: None,
        };
        let h = headers(&[("x-forwarded-for", "2001:db8::1, fd00::5")]);
        assert_eq!(
            resolve_client_ip(Some(ip("::1")), &h, &policy),
            Some(ip("2001:db8::1"))
        );
        let h = headers(&[("x-forwarded-for", "[2001:db8::2]:443")]);
        assert_eq!(
            resolve_client_ip(Some(ip("::1")), &h, &policy),
            Some(ip("2001:db8::2"))
        );
        assert_eq!(
            resolve_client_ip(Some(ip("::ffff:203.0.113.5")), &HeaderMap::new(), &policy),
            Some(ip("203.0.113.5"))
        );
    }

    #[test]
    fn requests_without_a_peer_fall_back_gracefully() {
        let policy = ClientIpPolicy {
            trusted_proxies: &[],
            internal_secret: None,
        };
        assert_eq!(resolve_client_ip(None, &HeaderMap::new(), &policy), None);
        let h = headers(&[("x-forwarded-for", "10.0.0.7")]);
        assert_eq!(resolve_client_ip(None, &h, &policy), Some(ip("10.0.0.7")));

        let req = Request::builder().body(()).unwrap();
        assert_eq!(ClientIpKeyExtractor.extract(&req).unwrap(), UNKNOWN_CLIENT);
    }
}
