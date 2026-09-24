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
//!
//! Passwords are Unicode-normalized (NFKC) before hashing, so the same
//! password typed on keyboards that compose accents differently (macOS vs
//! Android) matches. Hashes stored before normalization are still
//! accepted: verification also tries the password exactly as typed, and
//! [`verify_password_upgrading`] reports when the stored hash should be
//! replaced by one of the normalized form.

use crate::errors::api_error::{ApiError, codes};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum::http::StatusCode;
use serde_json::json;
use std::{
    borrow::Cow,
    sync::{Arc, LazyLock, OnceLock},
    time::Duration,
};
use tokio::sync::Semaphore;
use tracing::{error, warn};
use unicode_normalization::{UnicodeNormalization, is_nfkc_quick};

/// How long a request waits for a free hashing slot before giving up.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Concurrent Argon2 computations allowed process-wide.
static ARGON2_PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    let permits = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(2);
    Arc::new(Semaphore::new(permits))
});

/// The form of `password` that is hashed: NFKC.
pub fn normalize_password(password: &str) -> Cow<'_, str> {
    if matches!(
        is_nfkc_quick(password.chars()),
        unicode_normalization::IsNormalized::Yes
    ) {
        Cow::Borrowed(password)
    } else {
        Cow::Owned(password.nfkc().collect())
    }
}

/// Encrypt a password (blocking; prefer [`hash_password`] in handlers).
pub fn encrypt_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    Ok(argon2
        .hash_password(normalize_password(password).as_bytes(), &salt)
        .map(|hashed_password| hashed_password.to_string())?)
}

fn verify_exact(plain_password: &str, parsed_hash: &PasswordHash<'_>) -> bool {
    Argon2::default()
        .verify_password(plain_password.as_bytes(), parsed_hash)
        .is_ok()
}

/// Verifies `plain_password` against `hash`: `Ok(false)` when it matched
/// its normalized form (current hashes), `Ok(true)` when only the password
/// exactly as typed matched (a hash stored before normalization, which
/// should be replaced), `WrongPassword` otherwise.
pub fn verify_password_upgrading(plain_password: &str, hash: &str) -> Result<bool, ApiError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| {
        error!("Error parsing password hash: {e}");
        ApiError::WrongPassword
    })?;
    let normalized = normalize_password(plain_password);
    if verify_exact(&normalized, &parsed_hash) {
        return Ok(false);
    }
    // Legacy hash of a non-normalized password: only then is a second
    // (slow) check worth it.
    if normalized != plain_password && verify_exact(plain_password, &parsed_hash) {
        return Ok(true);
    }
    Err(ApiError::WrongPassword)
}

/// Verifies that a plaintext password matches the stored hash (blocking;
/// prefer [`verify_password_async`] in handlers).
pub fn verify_password(plain_password: &str, hash: &str) -> Result<(), ApiError> {
    verify_password_upgrading(plain_password, hash).map(|_| ())
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
///
/// The permit moves into the blocking task and is released only when the
/// computation ends: if the request is dropped meanwhile (client gone,
/// timeout), the Argon2 run keeps its slot, so the pool can never be
/// oversubscribed by abandoned requests.
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
            warn!("Password hashing is saturated; answering SERVICE_BUSY");
            return Err(service_busy());
        }
    };
    tokio::task::spawn_blocking(move || {
        let result = work();
        drop(permit);
        result
    })
    .await
    .map_err(|e| {
        error!(error = %e, "Password hashing task failed");
        ApiError::ServerError(axum::Error::new(e))
    })
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

/// [`verify_password_upgrading`] off the async runtime: `Ok(true)` when
/// the stored hash predates password normalization and should be
/// replaced.
pub async fn verify_password_upgrading_async(password: &str, hash: &str) -> Result<bool, ApiError> {
    let password = password.to_string();
    let hash = hash.to_string();
    run_limited(&ARGON2_PERMITS, ACQUIRE_TIMEOUT, move || {
        verify_password_upgrading(&password, &hash)
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
        let semaphore = Arc::new(Semaphore::new(0));
        let result = run_limited(&semaphore, Duration::from_millis(20), || 1).await;
        let err = result.unwrap_err();
        assert_eq!(err.code(), codes::SERVICE_BUSY);

        let semaphore = Arc::new(Semaphore::new(1));
        assert_eq!(
            run_limited(&semaphore, Duration::from_millis(20), || 7)
                .await
                .unwrap(),
            7
        );
        // The permit is released afterwards.
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn an_abandoned_request_keeps_its_slot_until_the_work_ends() {
        let semaphore = Arc::new(Semaphore::new(1));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let pending = run_limited(&semaphore, Duration::from_millis(20), move || {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        });
        // Start the work, then drop the request future.
        let _ = tokio::time::timeout(Duration::from_millis(50), pending).await;
        assert_eq!(semaphore.available_permits(), 0, "still hashing");
        tx.send(()).unwrap();
        for _ in 0..100 {
            if semaphore.available_permits() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[test]
    fn passwords_are_normalized_and_legacy_hashes_still_verify() {
        // "Ação" composed (NFC) and decomposed (NFD).
        let composed = "A\u{e7}\u{e3}o#2026x";
        let decomposed = "Ac\u{327}a\u{303}o#2026x";
        let hash = encrypt_password(decomposed).unwrap();
        assert!(!verify_password_upgrading(composed, &hash).unwrap());
        assert!(!verify_password_upgrading(decomposed, &hash).unwrap());

        // A hash of the raw decomposed form, made before normalization.
        let salt = SaltString::generate(&mut OsRng);
        let legacy = Argon2::default()
            .hash_password(decomposed.as_bytes(), &salt)
            .unwrap()
            .to_string();
        assert!(verify_password_upgrading(decomposed, &legacy).unwrap());
        assert!(verify_password_upgrading("wrong", &legacy).is_err());
    }
}
