//! Concurrency limit for PDF rendering.
//!
//! PDF layout is CPU-bound and allocation-heavy, and one of the export
//! endpoints is public. Every render runs on the blocking pool behind one
//! process-wide semaphore of [`PDF_PERMITS`]: excess requests wait up to
//! [`PDF_ACQUIRE_TIMEOUT`] and then fail fast with `SERVICE_BUSY` (503,
//! `meta.retry_after_seconds`) instead of queueing without bound.

use crate::errors::api_error::{ApiError, codes};
use axum::http::StatusCode;
use serde_json::json;
use std::{sync::LazyLock, time::Duration};
use tokio::sync::Semaphore;
use tracing::{error, warn};

/// PDFs rendered at the same time, process-wide.
pub const PDF_PERMITS: usize = 3;
/// How long a request waits for a free rendering slot.
pub const PDF_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

static PDF_SEMAPHORE: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(PDF_PERMITS));

/// The error returned when every rendering slot stayed busy.
pub fn service_busy() -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::SERVICE_UNAVAILABLE,
        codes::SERVICE_BUSY,
        "Too many exports are being generated right now. Please try again in a few seconds.",
        json!({ "retry_after_seconds": PDF_ACQUIRE_TIMEOUT.as_secs() }),
    )
}

/// Runs `work` on the blocking pool once a permit of `semaphore` is free,
/// or fails with `SERVICE_BUSY` after `timeout`.
pub async fn run_limited<T, F>(
    semaphore: &Semaphore,
    timeout: Duration,
    work: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = match tokio::time::timeout(timeout, semaphore.acquire()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) | Err(_) => {
            warn!("PDF rendering is saturated; answering SERVICE_BUSY");
            return Err(service_busy());
        }
    };
    let result = tokio::task::spawn_blocking(work).await.map_err(|e| {
        error!(error = %e, "PDF rendering task failed");
        ApiError::ServerError(axum::Error::new(e))
    });
    drop(permit);
    result
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
    use std::sync::Arc;

    #[tokio::test]
    async fn a_saturated_limiter_answers_service_busy() {
        let semaphore = Semaphore::new(0);
        let err = run_limited(&semaphore, Duration::from_millis(20), || 1)
            .await
            .unwrap_err();
        assert_eq!(err.code(), codes::SERVICE_BUSY);

        let semaphore = Semaphore::new(1);
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
}
