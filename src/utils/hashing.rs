use crate::errors::api_error::ApiError;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
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
pub fn verify_password(plain_password: &str, hash: &str) -> Result<bool, ApiError> {
    let parsed_hash = PasswordHash::new(hash).map_err(|e| {
        error!("Error parsing password hash: {e}");
        ApiError::WrongPassword
    })?;

    let argon2 = Argon2::default();

    argon2
        .verify_password(plain_password.as_bytes(), &parsed_hash)
        .map_err(|e| {
            error!("Error verifying password: {e}");
            ApiError::WrongPassword
        })
        .map(|_| true)
}
