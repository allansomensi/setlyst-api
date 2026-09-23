//! Password hashing (Argon2id).
//!
//! Argon2 is deliberately slow and memory-hungry. Running it on a Tokio
//! worker would stall every other request on that worker for the whole
//! computation, and a burst of sign-in attempts could starve the runtime.
//! So the async entry points ([`hash_password`], [`verify_password_async`],
//! [`dummy_verify_async`]) run the work on the blocking pool, behind a
//! global semaphore sized to the machine's parallelism: excess requests
//! wait up to [`ACQUIRE_TIMEOUT`] and then fail fast with `SERVICE_BUSY`
//! (503) instead of queueing without bound.
//!
//! The synchronous functions remain for command-line tools and tests.

use crate::errors::api_error::{ApiError, codes};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum::http::StatusCode;
use serde_json::json;
use std::{
    sync::{LazyLock, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use tracing::{error, warn};

/// How long a request waits for a free hashing slot before giving up.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Concurrent Argon2 computations allowed process-wide.
static ARGON2_PERMITS: LazyLock<Semaphore> = LazyLock::new(|| {
    let permits = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(2);
    Semaphore::new(permits)
});

/// Encrypt a password (blocking; prefer [`hash_password`] in handlers).
pub fn encrypt_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    Ok(argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hashed_password| hashed_password.to_string())?)
}

/// Verifies that a plaintext password matches the stored hash (blocking;
/// prefer [`verify_password_async`] in handlers).
pub fn verify_password(plain_password: &str, hash: &str) -> Result<(), ApiError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| {
        error!("Error parsing password hash: {e}");
        ApiError::WrongPassword
    })?;

    Argon2::default()
        .verify_password(plain_password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::WrongPassword)
}

fn dummy_hash() -> &'static str {
    static DUMMY_HASH: OnceLock<String> = OnceLock::new();
    DUMMY_HASH.get_or_init(|| encrypt_password("setlyst-timing-equalizer").unwrap_or_default())
}

/// Burns roughly the same time as a real verification. Called when a
/// sign-in names an account that doesn't exist, so response timing can't
/// be used to find out which usernames are registered.
pub fn dummy_verify(plain_password: &str) {
    let _ = verify_password(plain_password, dummy_hash());
}

/// The error returned when every hashing slot stayed busy.
pub fn service_busy() -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::SERVICE_UNAVAILABLE,
        codes::SERVICE_BUSY,
        "The service is busy. Please try again in a few seconds.",
        json!({ "retry_after_seconds": ACQUIRE_TIMEOUT.as_secs() }),
    )
}

/// Runs `work` on the blocking pool once a permit of `semaphore` is
/// available, or fails with `SERVICE_BUSY` after `timeout`. Generic over
/// the semaphore so the saturation path can be tested in isolation.
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
            warn!("Password hashing is saturated; answering SERVICE_BUSY");
            return Err(service_busy());
        }
    };
    let result = tokio::task::spawn_blocking(work).await.map_err(|e| {
        error!(error = %e, "Password hashing task failed");
        ApiError::ServerError(axum::Error::new(e))
    });
    drop(permit);
    result
}

/// Hashes `password` off the async runtime.
pub async fn hash_password(password: &str) -> Result<String, ApiError> {
    let password = password.to_string();
    run_limited(&ARGON2_PERMITS, ACQUIRE_TIMEOUT, move || {
        encrypt_password(&password)
    })
    .await?
}

/// Verifies `password` against `hash` off the async runtime.
pub async fn verify_password_async(password: &str, hash: &str) -> Result<(), ApiError> {
    let password = password.to_string();
    let hash = hash.to_string();
    run_limited(&ARGON2_PERMITS, ACQUIRE_TIMEOUT, move || {
        verify_password(&password, &hash)
    })
    .await?
}

/// [`dummy_verify`] off the async runtime.
pub async fn dummy_verify_async(password: &str) -> Result<(), ApiError> {
    let password = password.to_string();
    run_limited(&ARGON2_PERMITS, ACQUIRE_TIMEOUT, move || {
        dummy_verify(&password)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashing_round_trips_off_the_runtime() {
        let hash = hash_password("Str0ng!Passw0rd").await.unwrap();
        assert!(
            verify_password_async("Str0ng!Passw0rd", &hash)
                .await
                .is_ok()
        );
        assert!(matches!(
            verify_password_async("wrong", &hash).await,
            Err(ApiError::WrongPassword)
        ));
    }

    #[tokio::test]
    async fn a_saturated_pool_answers_service_busy() {
        let semaphore = Semaphore::new(0);
        let result = run_limited(&semaphore, Duration::from_millis(20), || 1).await;
        let err = result.unwrap_err();
        assert_eq!(err.code(), codes::SERVICE_BUSY);

        let semaphore = Semaphore::new(1);
        assert_eq!(
            run_limited(&semaphore, Duration::from_millis(20), || 7)
                .await
                .unwrap(),
            7
        );
        // The permit is released afterwards.
        assert_eq!(semaphore.available_permits(), 1);
    }
}
