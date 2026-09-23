use crate::errors::api_error::ApiError;
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use std::sync::OnceLock;
use tracing::error;

/// Encrypt a password.
pub fn encrypt_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    Ok(argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|hashed_password| hashed_password.to_string())?)
}

/// Verifies that a plaintext password matches the stored hash.
pub fn verify_password(plain_password: &str, hash: &str) -> Result<(), ApiError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| {
        error!("Error parsing password hash: {e}");
        ApiError::WrongPassword
    })?;

    Argon2::default()
        .verify_password(plain_password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::WrongPassword)
}

/// Burns roughly the same time as a real verification. Called when a
/// sign-in names an account that doesn't exist, so response timing can't
/// be used to find out which usernames are registered.
pub fn dummy_verify(plain_password: &str) {
    static DUMMY_HASH: OnceLock<String> = OnceLock::new();
    let hash =
        DUMMY_HASH.get_or_init(|| encrypt_password("setlyst-timing-equalizer").unwrap_or_default());
    let _ = verify_password(plain_password, hash);
}
