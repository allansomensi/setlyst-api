//! Automatic moderation of public profile data: usernames, avatars and
//! band logos. (Lyrics, setlist and gig names are deliberately out of
//! scope.)
//!
//! Checks never block the request that triggered them: they run in a
//! background task and, when something looks wrong, raise a
//! `moderation_flags` row for a human to review. The only hard refusal is
//! an exact slur in a username, enforced by the username validation.

pub mod image;
pub mod wordlist;

use crate::{
    config::Config,
    database::AppState,
    errors::api_error::ApiError,
    models::moderation::{ModerationSource, ModerationTarget, NewFlag},
};
use image::ImageVerdict;
use serde_json::json;
use tracing::{error, info};
use uuid::Uuid;

/// The outcome of checking a piece of text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextVerdict {
    /// `hate_term`, `sexual_term`, `offensive_term`.
    pub reasons: Vec<&'static str>,
    /// The normalized terms that matched.
    pub terms: Vec<String>,
    pub score: Option<f32>,
}

impl TextVerdict {
    pub fn is_clean(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// The checks behind automatic moderation, behind a trait so tests can
/// replace the network-bound parts.
#[async_trait::async_trait]
pub trait ModerationService: Send + Sync {
    /// Word-list check of a username or other short text.
    fn check_text(&self, text: &str) -> TextVerdict {
        let matches = wordlist::find_terms(text);
        let mut reasons: Vec<&'static str> = Vec::new();
        let mut score: Option<f32> = None;
        for m in &matches {
            if !reasons.contains(&m.kind.reason()) {
                reasons.push(m.kind.reason());
            }
            score = Some(score.unwrap_or(0.0).max(m.kind.score()));
        }
        TextVerdict {
            reasons,
            terms: matches.into_iter().map(|m| m.term).collect(),
            score,
        }
    }

    /// Checks an image URL (heuristics, then optional classification).
    async fn check_image_url(&self, url: &str) -> ImageVerdict;
}

/// Production implementation: word list, URL heuristics and, when
/// `MODERATION_VISION_API_KEY` is set, Google Cloud Vision SafeSearch.
pub struct DefaultModerationService {
    http: reqwest::Client,
}

impl DefaultModerationService {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for DefaultModerationService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ModerationService for DefaultModerationService {
    async fn check_image_url(&self, url: &str) -> ImageVerdict {
        let verdict = image::check_url_heuristics(url);
        if !verdict.is_clean() {
            return verdict;
        }
        match Config::try_get().and_then(|c| c.moderation_vision_api_key.as_deref()) {
            Some(key) => image::classify_with_vision(&self.http, key, url).await,
            None => verdict,
        }
    }
}

/// Heuristics only, never touching the network. Used by tests.
pub struct OfflineModerationService;

#[async_trait::async_trait]
impl ModerationService for OfflineModerationService {
    async fn check_image_url(&self, url: &str) -> ImageVerdict {
        image::check_url_heuristics(url)
    }
}

/// Checks a username and flags it when needed. Returns whether a new flag
/// was raised.
pub async fn review_username(
    state: &AppState,
    user_id: Uuid,
    username: &str,
) -> Result<bool, ApiError> {
    let verdict = state.moderation.check_text(username);
    if verdict.is_clean() {
        return Ok(false);
    }
    let created = state
        .moderation_repo
        .create_automatic(&NewFlag {
            target_type: ModerationTarget::Username,
            user_id,
            band_id: None,
            value: username.to_string(),
            reasons: verdict.reasons.iter().map(|r| r.to_string()).collect(),
            score: verdict.score,
            details: json!({ "terms": verdict.terms }),
            source: ModerationSource::Automatic,
            reported_by: None,
            report_note: None,
        })
        .await?;
    if created {
        info!(%user_id, "Username flagged for review");
    }
    Ok(created)
}

async fn review_image(
    state: &AppState,
    target_type: ModerationTarget,
    user_id: Uuid,
    band_id: Option<Uuid>,
    url: &str,
) -> Result<bool, ApiError> {
    let verdict = state.moderation.check_image_url(url).await;
    if verdict.is_clean() {
        return Ok(false);
    }
    let created = state
        .moderation_repo
        .create_automatic(&NewFlag {
            target_type,
            user_id,
            band_id,
            value: url.to_string(),
            reasons: verdict.reasons.iter().map(|r| r.to_string()).collect(),
            score: verdict.score,
            details: verdict.details,
            source: ModerationSource::Automatic,
            reported_by: None,
            report_note: None,
        })
        .await?;
    if created {
        info!(%user_id, ?band_id, target = target_type.key(), "Image flagged for review");
    }
    Ok(created)
}

/// Checks an avatar URL and flags it when needed.
pub async fn review_avatar(state: &AppState, user_id: Uuid, url: &str) -> Result<bool, ApiError> {
    review_image(state, ModerationTarget::Avatar, user_id, None, url).await
}

/// Checks a band logo URL. The flag is attributed to the band's owner.
pub async fn review_band_logo(
    state: &AppState,
    band_id: Uuid,
    url: &str,
) -> Result<bool, ApiError> {
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM band_members WHERE band_id = $1 AND role = 'owner' LIMIT 1",
    )
    .bind(band_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(owner) = owner else {
        return Ok(false);
    };
    review_image(state, ModerationTarget::BandLogo, owner, Some(band_id), url).await
}

/// [`review_username`] in the background.
pub fn spawn_username_review(state: &AppState, user_id: Uuid, username: &str) {
    let state = state.clone();
    let username = username.to_string();
    tokio::spawn(async move {
        if let Err(e) = review_username(&state, user_id, &username).await {
            error!(%user_id, error = %e, "Username moderation failed");
        }
    });
}

/// [`review_avatar`] in the background.
pub fn spawn_avatar_review(state: &AppState, user_id: Uuid, url: &str) {
    let state = state.clone();
    let url = url.to_string();
    tokio::spawn(async move {
        if let Err(e) = review_avatar(&state, user_id, &url).await {
            error!(%user_id, error = %e, "Avatar moderation failed");
        }
    });
}

/// [`review_band_logo`] in the background. Call it whenever a band logo is
/// set or changed.
pub fn spawn_band_logo_review(state: &AppState, band_id: Uuid, url: &str) {
    let state = state.clone();
    let url = url.to_string();
    tokio::spawn(async move {
        if let Err(e) = review_band_logo(&state, band_id, &url).await {
            error!(%band_id, error = %e, "Band logo moderation failed");
        }
    });
}

/// Re-checks every username, avatar and band logo on the platform.
/// Returns how many new flags were raised.
pub async fn rescan_all(state: &AppState) -> Result<i64, ApiError> {
    let mut flagged = 0i64;

    let users: Vec<(Uuid, String, Option<String>)> =
        sqlx::query_as("SELECT id, username, avatar_url FROM users ORDER BY created_at")
            .fetch_all(&state.db)
            .await?;
    for (id, username, avatar_url) in users {
        if review_username(state, id, &username).await? {
            flagged += 1;
        }
        if let Some(url) = avatar_url
            && review_avatar(state, id, &url).await?
        {
            flagged += 1;
        }
    }

    let bands: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, logo_url FROM bands WHERE logo_url IS NOT NULL")
            .fetch_all(&state.db)
            .await?;
    for (id, url) in bands {
        if review_band_logo(state, id, &url).await? {
            flagged += 1;
        }
    }

    Ok(flagged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_verdicts_aggregate_reasons() {
        let service = OfflineModerationService;
        let verdict = service.check_text("porn.fuck");
        assert!(verdict.reasons.contains(&"sexual_term"));
        assert!(verdict.reasons.contains(&"offensive_term"));
        assert_eq!(verdict.score, Some(0.9));
        assert!(service.check_text("joao.silva").is_clean());
    }

    #[tokio::test]
    async fn offline_image_checks_use_heuristics() {
        let service = OfflineModerationService;
        assert!(
            !service
                .check_image_url("https://pornhub.com/x.jpg")
                .await
                .is_clean()
        );
        assert!(
            service
                .check_image_url("https://i.imgur.com/x.jpg")
                .await
                .is_clean()
        );
    }
}
