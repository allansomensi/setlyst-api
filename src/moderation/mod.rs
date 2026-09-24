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
use sqlx::PgPool;
use std::{
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Image classifications (Vision calls) in flight at once, process-wide.
/// Checks run in background tasks; without a bound, a burst of avatar or
/// logo changes becomes thousands of concurrent 15-second HTTP calls.
pub const VISION_CONCURRENCY: usize = 4;
/// How long a check waits for a classification slot before giving up and
/// queueing the image for manual review instead.
const VISION_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(60);

static VISION_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(VISION_CONCURRENCY));

/// `platform_settings` key of the daily classification budget
/// (`{ "day": "YYYY-MM-DD", "used": n }`, UTC days).
const VISION_BUDGET_KEY: &str = "vision_budget";

/// The verdict for an image that couldn't be classified automatically (the
/// daily budget is spent, or every classification slot stayed busy): a
/// flag with reason `review_pending` puts it in front of a moderator.
pub fn review_pending(why: &'static str) -> ImageVerdict {
    ImageVerdict {
        reasons: vec!["review_pending"],
        score: None,
        details: json!({ "pending": why }),
    }
}

/// Takes one classification from today's budget. `false` when the budget
/// (`limit` per UTC day) is spent. Atomic across API instances: the
/// counter lives in `platform_settings` and is bumped by a single
/// conditional upsert.
pub async fn take_vision_budget(pool: &PgPool, limit: i64) -> Result<bool, ApiError> {
    let today = chrono::Utc::now().date_naive().to_string();
    let taken: Option<i32> = sqlx::query_scalar(
        "INSERT INTO platform_settings (key, value, updated_at)
         VALUES ($1, jsonb_build_object('day', $2::text, 'used', 1), NOW())
         ON CONFLICT (key) DO UPDATE SET
             value = CASE WHEN platform_settings.value->>'day' = $2
                          THEN jsonb_build_object('day', $2::text,
                                   'used', COALESCE((platform_settings.value->>'used')::bigint, 0) + 1)
                          ELSE jsonb_build_object('day', $2::text, 'used', 1) END,
             updated_at = NOW()
         WHERE platform_settings.value->>'day' IS DISTINCT FROM $2
            OR COALESCE((platform_settings.value->>'used')::bigint, 0) < $3
         RETURNING 1",
    )
    .bind(VISION_BUDGET_KEY)
    .bind(&today)
    .bind(limit)
    .fetch_optional(pool)
    .await?;
    Ok(taken.is_some() && limit > 0)
}

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
/// `MODERATION_VISION_API_KEY` is set, Google Cloud Vision SafeSearch —
/// at most [`VISION_CONCURRENCY`] calls at once and
/// `Config::vision_daily_budget` per UTC day (with a pool to keep the
/// count in; without one, the budget isn't enforced). Past either, the
/// image is flagged `review_pending` for a human instead.
pub struct DefaultModerationService {
    http: reqwest::Client,
    budget_pool: Option<PgPool>,
}

impl DefaultModerationService {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            budget_pool: None,
        }
    }

    /// Enforces the daily classification budget, counted in `pool`.
    pub fn with_budget(mut self, pool: PgPool) -> Self {
        self.budget_pool = Some(pool);
        self
    }

    async fn classify(&self, key: &str, url: &str) -> ImageVerdict {
        let Ok(Ok(_slot)) =
            tokio::time::timeout(VISION_ACQUIRE_TIMEOUT, VISION_SLOTS.acquire()).await
        else {
            warn!("Image classification is saturated; queued for manual review");
            return review_pending("classifier_busy");
        };
        if let Some(pool) = &self.budget_pool {
            let limit = Config::try_get().map_or(2_000, |c| c.vision_daily_budget);
            match take_vision_budget(pool, limit).await {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        limit,
                        "Daily image classification budget spent; queued for manual review"
                    );
                    return review_pending("daily_budget_spent");
                }
                Err(e) => {
                    error!(error = %e, "Could not read the image classification budget");
                    return review_pending("budget_unavailable");
                }
            }
        }
        image::classify_with_vision(&self.http, key, url).await
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
            Some(key) => self.classify(key, url).await,
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

/// Users (or bands) read per page by the rescans.
const RESCAN_PAGE: i64 = 500;

/// Re-checks every username (word list only, no network) and returns how
/// many new flags were raised. Pages through the users by id, so memory
/// stays bounded however many accounts there are.
pub async fn rescan_usernames(state: &AppState) -> Result<i64, ApiError> {
    let mut flagged = 0i64;
    let mut cursor = Uuid::nil();
    loop {
        let page: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, username FROM users WHERE id > $1 ORDER BY id LIMIT $2")
                .bind(cursor)
                .bind(RESCAN_PAGE)
                .fetch_all(&state.db)
                .await?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.0;
        for (id, username) in &page {
            if review_username(state, *id, username).await? {
                flagged += 1;
            }
        }
        if (page.len() as i64) < RESCAN_PAGE {
            break;
        }
    }
    Ok(flagged)
}

/// Whether this exact image was already looked at (a flag of any status,
/// open or resolved, for the same owner and URL): re-classifying it would
/// only bill the same answer again.
async fn already_reviewed(
    state: &AppState,
    target: ModerationTarget,
    user_id: Option<Uuid>,
    band_id: Option<Uuid>,
    url: &str,
) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM moderation_flags
                        WHERE target_type = $1::moderation_target AND value = $2
                          AND ($3::uuid IS NULL OR user_id = $3)
                          AND ($4::uuid IS NULL OR band_id = $4))",
    )
    .bind(target.key())
    .bind(url)
    .bind(user_id)
    .bind(band_id)
    .fetch_one(&state.db)
    .await?)
}

/// Re-checks every avatar and band logo, paging with an id cursor and
/// skipping images already reviewed. Returns how many new flags were
/// raised. Slow (network-bound): run it through [`spawn_image_rescan`].
pub async fn rescan_images(state: &AppState) -> Result<i64, ApiError> {
    let mut flagged = 0i64;

    let mut cursor = Uuid::nil();
    loop {
        let page: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, avatar_url FROM users
             WHERE id > $1 AND avatar_url IS NOT NULL ORDER BY id LIMIT $2",
        )
        .bind(cursor)
        .bind(RESCAN_PAGE)
        .fetch_all(&state.db)
        .await?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.0;
        for (id, url) in &page {
            if !already_reviewed(state, ModerationTarget::Avatar, Some(*id), None, url).await?
                && review_avatar(state, *id, url).await?
            {
                flagged += 1;
            }
        }
        if (page.len() as i64) < RESCAN_PAGE {
            break;
        }
    }

    let mut cursor = Uuid::nil();
    loop {
        let page: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, logo_url FROM bands
             WHERE id > $1 AND logo_url IS NOT NULL ORDER BY id LIMIT $2",
        )
        .bind(cursor)
        .bind(RESCAN_PAGE)
        .fetch_all(&state.db)
        .await?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = last.0;
        for (id, url) in &page {
            if !already_reviewed(state, ModerationTarget::BandLogo, None, Some(*id), url).await?
                && review_band_logo(state, *id, url).await?
            {
                flagged += 1;
            }
        }
        if (page.len() as i64) < RESCAN_PAGE {
            break;
        }
    }

    Ok(flagged)
}

/// Set while an image rescan runs, so repeated clicks don't start (and
/// bill) the same scan twice.
static IMAGE_RESCAN_RUNNING: AtomicBool = AtomicBool::new(false);

/// Starts [`rescan_images`] in the background unless one is already
/// running in this process. Returns whether a scan was started.
pub fn spawn_image_rescan(state: &AppState) -> bool {
    if IMAGE_RESCAN_RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    let state = state.clone();
    tokio::spawn(async move {
        match rescan_images(&state).await {
            Ok(flagged) => info!(flagged, "Image rescan finished"),
            Err(e) => error!(error = %e, "Image rescan failed"),
        }
        IMAGE_RESCAN_RUNNING.store(false, Ordering::SeqCst);
    });
    true
}

/// Re-checks every username, avatar and band logo on the platform, in the
/// calling task. Returns how many new flags were raised.
pub async fn rescan_all(state: &AppState) -> Result<i64, ApiError> {
    Ok(rescan_usernames(state).await? + rescan_images(state).await?)
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
