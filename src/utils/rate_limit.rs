//! A small in-memory sliding-window rate limiter keyed by anything
//! hashable (typically a user id), for per-account limits that the per-IP
//! `tower_governor` layers can't express.
//!
//! State is per process: with several API instances each enforces the
//! limit on its own share of the traffic, which is acceptable for abuse
//! protection (limits that must be exact are enforced in the database).

use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    sync::Mutex,
    time::{Duration, Instant},
};

pub struct SlidingWindowLimiter<K> {
    window: Duration,
    max: usize,
    hits: Mutex<HashMap<K, VecDeque<Instant>>>,
}

impl<K: Eq + Hash + Clone> SlidingWindowLimiter<K> {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            window,
            max,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Records a hit for `key`. `Err(retry_after)` when the limit is
    /// reached (the hit is not recorded then).
    pub fn check(&self, key: &K) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &K, now: Instant) -> Result<(), Duration> {
        let Ok(mut hits) = self.hits.lock() else {
            // A poisoned lock only means another thread panicked while
            // holding it; failing open is safer than failing every request.
            return Ok(());
        };
        // Occasional cleanup keeps the map from growing with idle keys.
        if hits.len() > 10_000 {
            let window = self.window;
            hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < window));
        }
        let queue = hits.entry(key.clone()).or_default();
        while queue
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            queue.pop_front();
        }
        if queue.len() >= self.max {
            let oldest = queue.front().copied().unwrap_or(now);
            return Err(self.window.saturating_sub(now.duration_since(oldest)));
        }
        queue.push_back(now);
        Ok(())
    }
}

/// Shared per-account limits for heavy endpoints, so every route (whoever
/// owns its controller) counts against the same window.
pub mod presets {
    use super::SlidingWindowLimiter;
    use std::{sync::LazyLock, time::Duration};
    use uuid::Uuid;

    const HOUR: Duration = Duration::from_secs(3600);

    /// `POST /backup/import`: 3 per account per hour. Each import is one
    /// long transaction.
    pub static BACKUP_IMPORT: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(3, HOUR));
    /// `GET /backup/export`: 10 per account per hour.
    pub static BACKUP_EXPORT: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(10, HOUR));
    /// `GET /songs/export/chordpro` (the whole library): 10 per account per
    /// hour.
    pub static CHORDPRO_EXPORT: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(10, HOUR));
    /// `GET /users/me/data-export` (LGPD): 10 per account per hour. Meant
    /// for the account controller:
    /// `presets::limit(&presets::DATA_EXPORT, access.user_id())?`.
    pub static DATA_EXPORT: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(10, HOUR));
    /// PDF exports by a signed-in account: 30 per minute.
    pub static PDF_EXPORT: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(30, Duration::from_secs(60)));
    /// Band membership changes by one account (role changes, removals):
    /// 60 per hour. Each one notifies the member concerned, so a loop of
    /// them is a notification (and e-mail) bombing tool.
    pub static BAND_MEMBER_CHANGES: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(60, HOUR));
    /// `GET /setlists/{id}/items`, a setlist's whole running order with
    /// every song's lyrics: 300 per 5 minutes per account. The offline
    /// sync reads it once per setlist; nothing legitimate reads it in a
    /// loop, and a filled-up repertoire makes it the heaviest answer the
    /// API produces.
    pub static SETLIST_ITEMS: LazyLock<SlidingWindowLimiter<Uuid>> =
        LazyLock::new(|| SlidingWindowLimiter::new(300, Duration::from_secs(5 * 60)));

    /// Records a hit of `user_id` on `limiter`; `TOO_MANY_ATTEMPTS` (429,
    /// `meta.retry_after_seconds`) when the window is full.
    pub fn limit(
        limiter: &SlidingWindowLimiter<Uuid>,
        user_id: Uuid,
    ) -> Result<(), crate::errors::api_error::ApiError> {
        limiter.check(&user_id).map_err(|retry| {
            crate::services::account::too_many_attempts(retry.as_secs().max(1) as i64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_within_the_window_and_recovers() {
        let limiter = SlidingWindowLimiter::new(2, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.check_at(&1, start).is_ok());
        assert!(limiter.check_at(&1, start).is_ok());
        let retry = limiter
            .check_at(&1, start + Duration::from_secs(10))
            .unwrap_err();
        assert_eq!(retry, Duration::from_secs(50));
        assert!(limiter.check_at(&2, start).is_ok(), "keys are independent");
        assert!(
            limiter
                .check_at(&1, start + Duration::from_secs(61))
                .is_ok()
        );
    }
}
