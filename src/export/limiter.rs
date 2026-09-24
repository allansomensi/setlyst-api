//! Concurrency and size limits for PDF rendering.
//!
//! PDF layout is CPU-bound and allocation-heavy (genpdf lays the whole
//! document out in memory: a 150-song songbook with long lyrics peaks at
//! gigabytes), and one of the export endpoints is public. So:
//!
//! - every render runs on the blocking pool behind one process-wide
//!   semaphore of [`PDF_PERMITS`]: excess requests wait up to
//!   [`PDF_ACQUIRE_TIMEOUT`] and then fail fast with `SERVICE_BUSY` (503,
//!   `meta.retry_after_seconds`) instead of queueing without bound. The
//!   permit is owned by the blocking task, not by the request: a request
//!   dropped by the global timeout can't free a slot while its render is
//!   still running;
//! - the size of what gets rendered is capped up front ([`ensure_pdf_fits`]):
//!   at most [`MAX_PDF_ITEMS`] items and, for a songbook, at most
//!   [`MAX_SONGBOOK_CHARS`] characters of lyrics and notes (`PDF_TOO_LARGE`,
//!   413).

use crate::{
    errors::api_error::{ApiError, codes},
    models::setlist::SetlistItem,
};
use axum::http::StatusCode;
use serde_json::json;
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use tracing::{error, warn};

/// PDFs rendered at the same time, process-wide.
pub const PDF_PERMITS: usize = 3;
/// How long a request waits for a free rendering slot.
pub const PDF_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Most items (songs, blocks and breaks) a setlist PDF may carry.
pub const MAX_PDF_ITEMS: usize = 200;
/// Most characters of lyrics (and performance notes) a songbook may carry:
/// roughly 100 pages, which renders in well under a second and a few tens
/// of megabytes.
pub const MAX_SONGBOOK_CHARS: usize = 250_000;

static PDF_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(PDF_PERMITS)));

/// The error returned when every rendering slot stayed busy.
pub fn service_busy() -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::SERVICE_UNAVAILABLE,
        codes::SERVICE_BUSY,
        "Too many exports are being generated right now. Please try again in a few seconds.",
        json!({ "retry_after_seconds": PDF_ACQUIRE_TIMEOUT.as_secs() }),
    )
}

/// `PDF_TOO_LARGE` (413) with what was over the limit.
pub fn pdf_too_large(message: impl Into<String>, meta: serde_json::Value) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::PAYLOAD_TOO_LARGE,
        codes::PDF_TOO_LARGE,
        message,
        meta,
    )
}

/// Refuses a setlist PDF that would be too big to render safely: more than
/// [`MAX_PDF_ITEMS`] items, or (with the songbook) more than
/// [`MAX_SONGBOOK_CHARS`] characters of lyrics and notes. Checked in the
/// handler, before a rendering slot is taken.
pub fn ensure_pdf_fits(items: &[SetlistItem], include_lyrics: bool) -> Result<(), ApiError> {
    if items.len() > MAX_PDF_ITEMS {
        return Err(pdf_too_large(
            format!("This setlist is too large to export (at most {MAX_PDF_ITEMS} items)."),
            json!({ "reason": "items", "limit": MAX_PDF_ITEMS }),
        ));
    }
    if include_lyrics {
        let total: usize = items
            .iter()
            .filter_map(|item| match item {
                SetlistItem::Song { song, .. } => Some(
                    song.lyrics.as_deref().map_or(0, str::len)
                        + song.performance_notes.as_deref().map_or(0, str::len),
                ),
                _ => None,
            })
            .sum();
        if total > MAX_SONGBOOK_CHARS {
            return Err(pdf_too_large(
                "This setlist is too large to export with lyrics; export without lyrics or split it.",
                json!({ "reason": "songbook", "limit": MAX_SONGBOOK_CHARS }),
            ));
        }
    }
    Ok(())
}

/// Runs `work` on the blocking pool once a permit of `semaphore` is free,
/// or fails with `SERVICE_BUSY` after `timeout`. The permit moves into the
/// blocking task and is released only when `work` returns, even if the
/// caller stopped waiting for it.
pub async fn run_limited<T, F>(
    semaphore: &Arc<Semaphore>,
    timeout: Duration,
    work: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = match tokio::time::timeout(timeout, semaphore.clone().acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) | Err(_) => {
            warn!("PDF rendering is saturated; answering SERVICE_BUSY");
            return Err(service_busy());
        }
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|e| {
        error!(error = %e, "PDF rendering task failed");
        ApiError::ServerError(axum::Error::new(e))
    })
}

/// Renders a PDF behind the global limit. Rendering errors are logged and
/// reported as a generic server error.
pub async fn render_pdf<F>(work: F) -> Result<Vec<u8>, ApiError>
where
    F: FnOnce() -> Result<Vec<u8>, genpdf::error::Error> + Send + 'static,
{
    run_limited(&PDF_SEMAPHORE, PDF_ACQUIRE_TIMEOUT, work)
        .await?
        .map_err(|e| {
            error!(error = ?e, "Failed to generate a PDF");
            ApiError::ServerError(axum::Error::new(std::io::Error::other(
                "Failed to generate PDF",
            )))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::song::SongWithArtist;

    #[tokio::test]
    async fn a_saturated_limiter_answers_service_busy() {
        let semaphore = Arc::new(Semaphore::new(0));
        let err = run_limited(&semaphore, Duration::from_millis(20), || 1)
            .await
            .unwrap_err();
        assert_eq!(err.code(), codes::SERVICE_BUSY);

        let semaphore = Arc::new(Semaphore::new(1));
        assert_eq!(
            run_limited(&semaphore, Duration::from_millis(20), || 7)
                .await
                .unwrap(),
            7
        );
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn at_most_the_permitted_renders_run_at_once() {
        let semaphore = Arc::new(Semaphore::new(2));
        let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let semaphore = semaphore.clone();
            let running = running.clone();
            let peak = peak.clone();
            handles.push(tokio::spawn(async move {
                run_limited(&semaphore, Duration::from_secs(5), move || {
                    let now = running.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
                .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
        assert_eq!(service_busy().code(), codes::SERVICE_BUSY);
    }

    #[tokio::test]
    async fn an_abandoned_request_keeps_its_slot_until_the_render_ends() {
        let semaphore = Arc::new(Semaphore::new(1));
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let task = {
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                run_limited(&semaphore, Duration::from_secs(1), move || {
                    let _ = wait.recv();
                })
                .await
            })
        };
        // Let the render start, then drop the request (as the timeout layer
        // would): the render is still running, so the slot stays taken.
        while semaphore.available_permits() == 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(semaphore.available_permits(), 0);
        release.send(()).unwrap();
        for _ in 0..100 {
            if semaphore.available_permits() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(semaphore.available_permits(), 1);
    }

    fn song(lyrics: usize) -> SetlistItem {
        let mut song: SongWithArtist = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "title": "t",
            "artist_id": uuid::Uuid::new_v4(),
            "artist_name": "a",
            "user_id": uuid::Uuid::new_v4(),
            "band_id": null,
            "forked_from": null,
            "tempo": null,
            "lyrics": null,
            "tonality": null,
            "genre": null,
            "duration": null,
            "links": [],
            "tags": [],
            "created_at": "2026-01-01T00:00:00",
            "updated_at": "2026-01-01T00:00:00",
        }))
        .expect("minimal song");
        song.lyrics = Some("x".repeat(lyrics));
        SetlistItem::Song {
            position: 1,
            song: Box::new(song),
        }
    }

    #[test]
    fn oversized_pdfs_are_refused_up_front() {
        let items: Vec<SetlistItem> = (0..MAX_PDF_ITEMS).map(|_| song(10)).collect();
        assert!(ensure_pdf_fits(&items, true).is_ok());

        let too_many: Vec<SetlistItem> = (0..=MAX_PDF_ITEMS).map(|_| song(0)).collect();
        let err = ensure_pdf_fits(&too_many, false).unwrap_err();
        assert_eq!(err.code(), codes::PDF_TOO_LARGE);

        let heavy: Vec<SetlistItem> = (0..6).map(|_| song(50_000)).collect();
        assert_eq!(
            ensure_pdf_fits(&heavy, true).unwrap_err().code(),
            codes::PDF_TOO_LARGE
        );
        // Without the songbook the lyrics don't matter.
        assert!(ensure_pdf_fits(&heavy, false).is_ok());
    }
}
