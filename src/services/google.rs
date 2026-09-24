//! Verification of Google ID tokens ("Sign in with Google").
//!
//! The web client obtains an ID token from Google Identity Services and
//! hands it to `POST /auth/oauth/google`. It is only trusted after local
//! verification: RS256 signature against Google's published keys (JWKS,
//! cached for as long as Google's `Cache-Control: max-age` allows and
//! refreshed when an unknown key id shows up), issuer, audience (one of
//! `GOOGLE_CLIENT_IDS`), expiry, and a verified e-mail address.
//!
//! The verifier sits behind [`GoogleTokenVerifier`] so tests can inject a
//! fake instead of reaching Google.

use crate::config::Config;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

const JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const ISSUERS: [&str; 2] = ["accounts.google.com", "https://accounts.google.com"];
/// Used when Google's response has no usable `max-age`.
const DEFAULT_CACHE: Duration = Duration::from_secs(3600);
/// Minimum time between two key refreshes triggered by unknown key ids,
/// so forged tokens can't make us hammer Google.
const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
/// How long keys fetched earlier keep being used while Google's key
/// endpoint can't be reached (Google publishes keys well before signing
/// with them and keeps retired ones for days), so an outage there doesn't
/// stop every Google sign-in.
const MAX_STALE: Duration = Duration::from_secs(24 * 3600);
/// While serving stale keys, how long to wait before trying Google again.
const STALE_RETRY: Duration = Duration::from_secs(300);

/// What a verified token says about its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleIdentity {
    /// Google's stable account id.
    pub subject: String,
    pub email: String,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    /// The Google Workspace domain of the account (`hd` claim), if any.
    pub hosted_domain: Option<String>,
}

impl GoogleIdentity {
    /// Whether Google is authoritative for the address, so it can be
    /// trusted to link an existing account by e-mail: a Gmail address, or
    /// a Workspace account whose domain is the address's domain. For any
    /// other address `email_verified` only means Google checked it once,
    /// when the Google account was created (a lapsed domain or a recycled
    /// mailbox could have changed hands since).
    pub fn is_authoritative_for_email(&self) -> bool {
        let email = self.email.trim().to_ascii_lowercase();
        let Some((_, domain)) = email.rsplit_once('@') else {
            return false;
        };
        matches!(domain, "gmail.com" | "googlemail.com")
            || self
                .hosted_domain
                .as_deref()
                .is_some_and(|hd| hd.trim().eq_ignore_ascii_case(domain))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoogleVerifyError {
    /// Google sign-in is not configured.
    Disabled,
    /// The token was rejected (reason for the logs only).
    Invalid(String),
}

#[async_trait::async_trait]
pub trait GoogleTokenVerifier: Send + Sync {
    /// `false` when Google sign-in is not configured.
    fn enabled(&self) -> bool;
    async fn verify(&self, id_token: &str) -> Result<GoogleIdentity, GoogleVerifyError>;
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
    #[serde(default)]
    kty: String,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    String(String),
}

impl BoolOrString {
    fn is_true(&self) -> bool {
        match self {
            BoolOrString::Bool(b) => *b,
            BoolOrString::String(s) => s == "true",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GoogleClaims {
    sub: String,
    email: Option<String>,
    email_verified: Option<BoolOrString>,
    name: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
    hd: Option<String>,
}

struct KeyCache {
    /// `kid` → `(n, e)`.
    keys: HashMap<String, (String, String)>,
    expires_at: Instant,
    fetched_at: Instant,
}

impl KeyCache {
    /// Key `kid` from an expired cache when refreshing failed: known and
    /// fetched less than [`MAX_STALE`] ago. The cache then counts as fresh
    /// for [`STALE_RETRY`], so a Google outage isn't retried on every
    /// sign-in.
    fn stale_key(&mut self, kid: &str, now: Instant) -> Option<(String, String)> {
        if now.saturating_duration_since(self.fetched_at) >= MAX_STALE {
            return None;
        }
        let key = self.keys.get(kid).cloned()?;
        self.expires_at = now + STALE_RETRY;
        Some(key)
    }
}

/// Production verifier: Google's JWKS over HTTPS.
pub struct GoogleJwksVerifier {
    http: reqwest::Client,
    cache: RwLock<Option<KeyCache>>,
}

impl GoogleJwksVerifier {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            cache: RwLock::new(None),
        }
    }

    fn client_ids() -> Vec<String> {
        Config::try_get()
            .map(|c| c.google_client_ids.clone())
            .unwrap_or_default()
    }

    async fn fetch_keys(&self) -> Result<KeyCache, GoogleVerifyError> {
        let response = self
            .http
            .get(JWKS_URL)
            .send()
            .await
            .map_err(|e| GoogleVerifyError::Invalid(format!("JWKS request failed: {e}")))?;
        let max_age = response
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_max_age)
            .unwrap_or(DEFAULT_CACHE);
        let jwks: Jwks = response
            .json()
            .await
            .map_err(|e| GoogleVerifyError::Invalid(format!("JWKS unreadable: {e}")))?;
        let now = Instant::now();
        Ok(KeyCache {
            keys: jwks
                .keys
                .into_iter()
                .filter(|k| k.kty.is_empty() || k.kty == "RSA")
                .map(|k| (k.kid, (k.n, k.e)))
                .collect(),
            expires_at: now + max_age,
            fetched_at: now,
        })
    }

    /// The `(n, e)` components of key `kid`, refreshing the cache when it
    /// expired or doesn't know the key.
    async fn key(&self, kid: &str) -> Result<(String, String), GoogleVerifyError> {
        {
            let cache = self.cache.read().await;
            if let Some(cache) = cache.as_ref() {
                let fresh = cache.expires_at > Instant::now();
                if fresh && let Some(key) = cache.keys.get(kid) {
                    return Ok(key.clone());
                }
                if fresh && cache.fetched_at.elapsed() < MIN_REFRESH_INTERVAL {
                    return Err(GoogleVerifyError::Invalid(format!("unknown key id {kid}")));
                }
            }
        }
        let mut cache = self.cache.write().await;
        // Another request may have refreshed while we waited.
        if let Some(existing) = cache.as_ref()
            && existing.expires_at > Instant::now()
            && let Some(key) = existing.keys.get(kid)
        {
            return Ok(key.clone());
        }
        debug!("Refreshing Google signing keys");
        let fresh = match self.fetch_keys().await {
            Ok(fresh) => fresh,
            Err(e) => {
                if let Some(key) = cache
                    .as_mut()
                    .and_then(|existing| existing.stale_key(kid, Instant::now()))
                {
                    warn!(error = ?e, "Could not refresh Google's signing keys; using the previous ones");
                    return Ok(key);
                }
                return Err(e);
            }
        };
        let key = fresh.keys.get(kid).cloned();
        *cache = Some(fresh);
        key.ok_or_else(|| GoogleVerifyError::Invalid(format!("unknown key id {kid}")))
    }
}

impl Default for GoogleJwksVerifier {
    fn default() -> Self {
        Self::new()
    }
}

/// `max-age` of a `Cache-Control` header value.
pub fn parse_max_age(value: &str) -> Option<Duration> {
    value
        .split(',')
        .map(str::trim)
        .find_map(|directive| directive.strip_prefix("max-age="))
        .and_then(|seconds| seconds.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[async_trait::async_trait]
impl GoogleTokenVerifier for GoogleJwksVerifier {
    fn enabled(&self) -> bool {
        !Self::client_ids().is_empty()
    }

    async fn verify(&self, id_token: &str) -> Result<GoogleIdentity, GoogleVerifyError> {
        let client_ids = Self::client_ids();
        if client_ids.is_empty() {
            return Err(GoogleVerifyError::Disabled);
        }

        let header = decode_header(id_token)
            .map_err(|e| GoogleVerifyError::Invalid(format!("malformed token: {e}")))?;
        if header.alg != Algorithm::RS256 {
            return Err(GoogleVerifyError::Invalid("unexpected algorithm".into()));
        }
        let kid = header
            .kid
            .ok_or_else(|| GoogleVerifyError::Invalid("missing key id".into()))?;
        let (n, e) = self.key(&kid).await?;
        let key = DecodingKey::from_rsa_components(&n, &e)
            .map_err(|e| GoogleVerifyError::Invalid(format!("bad key: {e}")))?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&client_ids);
        validation.set_issuer(&ISSUERS);
        validation.leeway = 30;
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

        let claims = decode::<GoogleClaims>(id_token, &key, &validation)
            .map_err(|e| {
                warn!(error = %e, "Rejected a Google ID token");
                GoogleVerifyError::Invalid(e.to_string())
            })?
            .claims;

        identity_from_claims(claims)
    }
}

fn identity_from_claims(claims: GoogleClaims) -> Result<GoogleIdentity, GoogleVerifyError> {
    let email = claims
        .email
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| GoogleVerifyError::Invalid("no e-mail in token".into()))?;
    if !claims.email_verified.is_some_and(|v| v.is_true()) {
        return Err(GoogleVerifyError::Invalid(
            "e-mail not verified by Google".into(),
        ));
    }
    if claims.sub.is_empty() || claims.sub.len() > 255 {
        return Err(GoogleVerifyError::Invalid("bad subject".into()));
    }
    Ok(GoogleIdentity {
        subject: claims.sub,
        email: email.trim().to_string(),
        name: claims.name,
        given_name: claims.given_name,
        family_name: claims.family_name,
        hosted_domain: claims.hd.filter(|hd| !hd.trim().is_empty()),
    })
}

/// Test double: accepts tokens of the form `fake:<sub>:<email>[:<name>]`
/// and anything registered with [`FakeGoogleVerifier::with`]. A name
/// ending in `@hd=<domain>` sets the Workspace domain.
#[derive(Default)]
pub struct FakeGoogleVerifier {
    identities: std::sync::Mutex<HashMap<String, GoogleIdentity>>,
}

impl FakeGoogleVerifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `token` verify as `identity`.
    pub fn with(self, token: &str, identity: GoogleIdentity) -> Self {
        if let Ok(mut map) = self.identities.lock() {
            map.insert(token.to_string(), identity);
        }
        self
    }
}

#[async_trait::async_trait]
impl GoogleTokenVerifier for FakeGoogleVerifier {
    fn enabled(&self) -> bool {
        true
    }

    async fn verify(&self, id_token: &str) -> Result<GoogleIdentity, GoogleVerifyError> {
        if let Some(identity) = self
            .identities
            .lock()
            .ok()
            .and_then(|m| m.get(id_token).cloned())
        {
            return Ok(identity);
        }
        let mut parts = id_token.splitn(4, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("fake"), Some(sub), Some(email)) if !sub.is_empty() && email.contains('@') => {
                let (name, hosted_domain) = match parts.next() {
                    Some(rest) => match rest.split_once("@hd=") {
                        Some((name, hd)) => (
                            (!name.is_empty()).then(|| name.to_string()),
                            Some(hd.to_string()),
                        ),
                        None => (Some(rest.to_string()), None),
                    },
                    None => (None, None),
                };
                Ok(GoogleIdentity {
                    subject: sub.to_string(),
                    email: email.to_string(),
                    name,
                    given_name: None,
                    family_name: None,
                    hosted_domain,
                })
            }
            _ => Err(GoogleVerifyError::Invalid(
                "fake verifier rejected the token".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_keys_are_served_for_a_day_then_dropped() {
        let fetched = Instant::now();
        let mut cache = KeyCache {
            keys: HashMap::from([("k1".to_string(), ("n".to_string(), "e".to_string()))]),
            expires_at: fetched + Duration::from_secs(60),
            fetched_at: fetched,
        };
        let later = fetched + Duration::from_secs(3 * 3600);
        assert_eq!(cache.stale_key("unknown", later), None);
        assert_eq!(
            cache.stale_key("k1", later),
            Some(("n".to_string(), "e".to_string()))
        );
        assert_eq!(cache.expires_at, later + STALE_RETRY, "retried later");
        assert_eq!(cache.stale_key("k1", fetched + MAX_STALE), None);
    }

    #[test]
    fn cache_control_max_age() {
        assert_eq!(
            parse_max_age("public, max-age=19204, must-revalidate, no-transform"),
            Some(Duration::from_secs(19204))
        );
        assert_eq!(parse_max_age("no-cache"), None);
    }

    #[test]
    fn claims_need_a_verified_email() {
        let claims = |verified: Option<BoolOrString>| GoogleClaims {
            sub: "123".into(),
            email: Some("ana@example.com".into()),
            email_verified: verified,
            name: Some("Ana".into()),
            given_name: None,
            family_name: None,
            hd: None,
        };
        assert!(identity_from_claims(claims(Some(BoolOrString::Bool(true)))).is_ok());
        assert!(identity_from_claims(claims(Some(BoolOrString::String("true".into())))).is_ok());
        assert!(identity_from_claims(claims(Some(BoolOrString::Bool(false)))).is_err());
        assert!(identity_from_claims(claims(None)).is_err());
    }

    #[tokio::test]
    async fn fake_verifier_parses_test_tokens() {
        let fake = FakeGoogleVerifier::new();
        let identity = fake
            .verify("fake:sub-1:ana@example.com:Ana Maria")
            .await
            .unwrap();
        assert_eq!(identity.subject, "sub-1");
        assert_eq!(identity.name.as_deref(), Some("Ana Maria"));
        assert!(fake.verify("real-looking-token").await.is_err());
        let workspace = fake
            .verify("fake:sub-2:ceo@startup.example:@hd=startup.example")
            .await
            .unwrap();
        assert_eq!(workspace.hosted_domain.as_deref(), Some("startup.example"));
        assert!(workspace.name.is_none());
    }

    #[test]
    fn only_gmail_or_a_matching_workspace_domain_is_authoritative() {
        let identity = |email: &str, hd: Option<&str>| GoogleIdentity {
            subject: "s".into(),
            email: email.into(),
            name: None,
            given_name: None,
            family_name: None,
            hosted_domain: hd.map(str::to_string),
        };
        assert!(identity("ana@gmail.com", None).is_authoritative_for_email());
        assert!(identity("Ana@GoogleMail.com", None).is_authoritative_for_email());
        assert!(
            identity("ceo@startup.example", Some("startup.example")).is_authoritative_for_email()
        );
        assert!(!identity("ceo@startup.example", None).is_authoritative_for_email());
        assert!(
            !identity("ceo@startup.example", Some("other.example")).is_authoritative_for_email()
        );
    }

    #[tokio::test]
    async fn the_real_verifier_is_disabled_without_client_ids() {
        let verifier = GoogleJwksVerifier::new();
        if Config::try_get().is_none() {
            assert!(!verifier.enabled());
            assert_eq!(
                verifier.verify("x.y.z").await.unwrap_err(),
                GoogleVerifyError::Disabled
            );
        }
    }
}
