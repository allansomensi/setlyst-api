//! Image URL moderation.
//!
//! Avatars and band logos are links to externally hosted images. Two
//! layers look at them:
//!
//! 1. **Heuristics** on the URL itself: hosts on a list of well-known
//!    adult sites (and adult-only TLDs) and suggestive keywords in the host
//!    or path. Blocked hosts are refused at validation time; keywords only
//!    raise a flag.
//! 2. **Image classification** (optional): Google Cloud Vision SafeSearch,
//!    used only when `MODERATION_VISION_API_KEY` is set. A flag is raised
//!    when adult, racy or violent content is at least `LIKELY`.

use serde_json::{Value, json};
use tracing::warn;

/// Well-known adult sites. A host matches when it is one of these or a
/// subdomain of one.
pub const BLOCKED_DOMAINS: &[&str] = &[
    "pornhub.com",
    "xvideos.com",
    "xnxx.com",
    "xhamster.com",
    "xhamsterlive.com",
    "redtube.com",
    "youporn.com",
    "tube8.com",
    "spankbang.com",
    "eporner.com",
    "beeg.com",
    "porn.com",
    "sex.com",
    "youjizz.com",
    "4tube.com",
    "porntrex.com",
    "motherless.com",
    "onlyfans.com",
    "fansly.com",
    "manyvids.com",
    "clips4sale.com",
    "chaturbate.com",
    "stripchat.com",
    "bongacams.com",
    "livejasmin.com",
    "cam4.com",
    "myfreecams.com",
    "brazzers.com",
    "realitykings.com",
    "bangbros.com",
    "naughtyamerica.com",
    "playboy.com",
    "penthouse.com",
    "hustler.com",
    "adultfriendfinder.com",
    "literotica.com",
    "erome.com",
    "redgifs.com",
    "imagefap.com",
    "nhentai.net",
    "e-hentai.org",
    "gelbooru.com",
    "danbooru.donmai.us",
    "sankakucomplex.com",
    "rule34.xxx",
];

/// Top-level domains reserved for adult content.
const BLOCKED_TLDS: &[&str] = &["xxx", "porn", "sex", "adult"];

/// Keywords matched anywhere in the host or path.
const SUBSTRING_KEYWORDS: &[&str] = &["porn", "xxx", "nsfw", "hentai"];
/// Keywords matched as whole words of the host or path.
const WORD_KEYWORDS: &[&str] = &[
    "sex", "sexy", "nude", "nudes", "naked", "erotic", "erotica", "fetish", "gore", "boobs",
];

/// `true` when `host` is (a subdomain of) a blocked adult site or uses an
/// adult-only TLD.
pub fn is_blocked_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Some(tld) = host.rsplit('.').next()
        && BLOCKED_TLDS.contains(&tld)
    {
        return true;
    }
    BLOCKED_DOMAINS
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The outcome of checking an image.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageVerdict {
    /// Machine-readable reasons (`blocked_domain`, `suspicious_url`,
    /// `nsfw_image`, `violent_image`); empty when nothing was found.
    pub reasons: Vec<&'static str>,
    pub score: Option<f32>,
    pub details: Value,
}

impl ImageVerdict {
    pub fn is_clean(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// URL-only heuristics. Never touches the network.
pub fn check_url_heuristics(url: &str) -> ImageVerdict {
    let lower = url.to_ascii_lowercase();
    let without_scheme = lower
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&lower);
    let (host, path) = without_scheme
        .split_once('/')
        .unwrap_or((without_scheme, ""));
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);

    if is_blocked_host(host) {
        return ImageVerdict {
            reasons: vec!["blocked_domain"],
            score: Some(1.0),
            details: json!({ "host": host }),
        };
    }

    let haystack = format!("{host}/{path}");
    let mut keywords: Vec<&str> = SUBSTRING_KEYWORDS
        .iter()
        .copied()
        .filter(|k| haystack.contains(k))
        .collect();
    let words: Vec<&str> = haystack
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    keywords.extend(WORD_KEYWORDS.iter().copied().filter(|k| words.contains(k)));

    if keywords.is_empty() {
        ImageVerdict::default()
    } else {
        ImageVerdict {
            reasons: vec!["suspicious_url"],
            score: Some(0.6),
            details: json!({ "keywords": keywords }),
        }
    }
}

/// SafeSearch likelihoods, weakest to strongest.
fn likelihood_rank(value: &str) -> u8 {
    match value {
        "VERY_UNLIKELY" => 1,
        "UNLIKELY" => 2,
        "POSSIBLE" => 3,
        "LIKELY" => 4,
        "VERY_LIKELY" => 5,
        _ => 0,
    }
}

/// Interprets a Vision `images:annotate` response.
pub fn verdict_from_safe_search(response: &Value) -> ImageVerdict {
    let annotation = &response["responses"][0]["safeSearchAnnotation"];
    let rank = |key: &str| likelihood_rank(annotation[key].as_str().unwrap_or_default());
    let (adult, racy, violence) = (rank("adult"), rank("racy"), rank("violence"));

    let mut reasons = Vec::new();
    if adult >= 4 || racy >= 4 {
        reasons.push("nsfw_image");
    }
    if violence >= 4 {
        reasons.push("violent_image");
    }
    let strongest = adult.max(racy).max(violence);
    ImageVerdict {
        score: (!reasons.is_empty()).then_some(strongest as f32 / 5.0),
        reasons,
        details: json!({ "safe_search": annotation }),
    }
}

/// The SafeSearch request for `url`. The key goes in a header, never in
/// the URL: reqwest errors (and proxies, and access logs) carry the URL,
/// so a query-string key would end up in our logs.
fn vision_request(client: &reqwest::Client, api_key: &str, url: &str) -> reqwest::RequestBuilder {
    let body = json!({
        "requests": [{
            "image": { "source": { "imageUri": url } },
            "features": [{ "type": "SAFE_SEARCH_DETECTION" }]
        }]
    });
    client
        .post("https://vision.googleapis.com/v1/images:annotate")
        .header("x-goog-api-key", api_key)
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
}

/// Calls Google Cloud Vision SafeSearch on `url`. Any failure (network,
/// quota, unreadable image) yields a clean verdict with the error in the
/// details: classification is a best-effort extra, not a gate.
pub async fn classify_with_vision(
    client: &reqwest::Client,
    api_key: &str,
    url: &str,
) -> ImageVerdict {
    let result = vision_request(client, api_key, url).send().await;
    match result {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(json) => verdict_from_safe_search(&json),
            Err(e) => {
                warn!(error = %e.without_url(), "Vision response could not be parsed");
                ImageVerdict::default()
            }
        },
        Ok(response) => {
            warn!(status = %response.status(), "Vision request failed");
            ImageVerdict::default()
        }
        Err(e) => {
            warn!(error = %e.without_url(), "Vision request failed");
            ImageVerdict::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vision_key_never_appears_in_the_url() {
        let client = reqwest::Client::new();
        let request = vision_request(&client, "secret-vision-key", "https://img.test/a.png")
            .build()
            .unwrap();
        assert!(!request.url().as_str().contains("secret-vision-key"));
        assert!(request.url().query().is_none());
        assert_eq!(
            request.headers().get("x-goog-api-key").unwrap(),
            "secret-vision-key"
        );
    }

    #[test]
    fn blocked_hosts_and_subdomains() {
        assert!(is_blocked_host("pornhub.com"));
        assert!(is_blocked_host("ei.PORNHUB.com"));
        assert!(is_blocked_host("anything.xxx"));
        assert!(!is_blocked_host("notpornhub.com"));
        assert!(!is_blocked_host("images.unsplash.com"));
        assert!(!is_blocked_host("sussex.ac.uk"));
    }

    #[test]
    fn heuristics_flag_blocked_domains_and_keywords() {
        assert_eq!(
            check_url_heuristics("https://cdn.xvideos.com/a.jpg").reasons,
            vec!["blocked_domain"]
        );
        assert_eq!(
            check_url_heuristics("https://img.example.com/nsfw/1.png").reasons,
            vec!["suspicious_url"]
        );
        assert_eq!(
            check_url_heuristics("https://example.com/photos/sexy-pic.jpg").reasons,
            vec!["suspicious_url"]
        );
        assert!(check_url_heuristics("https://i.imgur.com/abc123.png").is_clean());
        assert!(check_url_heuristics("https://www.sussex.ac.uk/logo.png").is_clean());
        assert!(check_url_heuristics("https://example.com/sextet-band.jpg").is_clean());
    }

    #[test]
    fn safe_search_thresholds() {
        let response = json!({ "responses": [{ "safeSearchAnnotation": {
            "adult": "LIKELY", "racy": "POSSIBLE", "violence": "VERY_UNLIKELY"
        }}]});
        assert_eq!(
            verdict_from_safe_search(&response).reasons,
            vec!["nsfw_image"]
        );

        let response = json!({ "responses": [{ "safeSearchAnnotation": {
            "adult": "UNLIKELY", "racy": "POSSIBLE", "violence": "VERY_LIKELY"
        }}]});
        assert_eq!(
            verdict_from_safe_search(&response).reasons,
            vec!["violent_image"]
        );

        let response = json!({ "responses": [{ "safeSearchAnnotation": {
            "adult": "POSSIBLE", "racy": "POSSIBLE", "violence": "UNLIKELY"
        }}]});
        assert!(verdict_from_safe_search(&response).is_clean());
        assert!(verdict_from_safe_search(&json!({})).is_clean());
    }
}
