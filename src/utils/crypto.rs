//! Symmetric cryptography helpers.
//!
//! - **Encryption at rest** (AES-256-GCM) for secrets the server must be
//!   able to read back, such as TOTP seeds. Values are stored as
//!   `v1:` + base64(nonce ‖ ciphertext) so the scheme can be rotated later
//!   without guessing what an old row contains.
//! - **Keyed hashes** (HMAC-SHA256) for one-time codes: the database only
//!   ever holds a MAC, so a leaked table does not reveal a usable code, and
//!   a 6-digit code can't be brute-forced offline without the server key.
//! - **Opaque tokens** (random, URL-safe) and their SHA-256 digests, for
//!   values handed to clients that are looked up later (login challenges).
//!
//! Every key is derived from configuration with HKDF-SHA256 under a
//! distinct label, so one secret never serves two purposes.

use crate::{config::Config, errors::api_error::ApiError};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as BASE64URL},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::error;

type HmacSha256 = Hmac<Sha256>;

/// Prefix of the current at-rest encryption format.
const ENCRYPTION_VERSION: &str = "v1:";
const NONCE_LEN: usize = 12;

/// HKDF labels. Never change one: every stored value keyed with it would
/// become unreadable.
pub const LABEL_CODES: &str = "setlyst-codes";
pub const LABEL_DATA_KEY: &str = "setlyst-data-key";
pub const LABEL_UNSUBSCRIBE: &str = "setlyst-unsubscribe";

/// Derives a 32-byte key from `secret` for `label` (HKDF-SHA256, no salt:
/// the input is already a high-entropy secret).
pub fn derive_key(secret: &[u8], label: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, secret);
    let mut out = [0u8; 32];
    // 32 bytes is far below HKDF's 255 * 32 limit, so this can't fail.
    if hk.expand(label.as_bytes(), &mut out).is_err() {
        error!("HKDF expansion failed");
    }
    out
}

/// The server secret everything else is derived from. Falls back to a
/// fixed development value when no configuration is loaded (unit tests).
fn jwt_secret() -> Vec<u8> {
    Config::try_get()
        .map(|c| c.jwt_secret.as_bytes().to_vec())
        .unwrap_or_else(|| b"setlyst-unit-test-secret-not-for-production".to_vec())
}

/// Key for one-time code MACs.
fn codes_key() -> [u8; 32] {
    derive_key(&jwt_secret(), LABEL_CODES)
}

/// Key for encryption at rest: `DATA_ENCRYPTION_KEY`, or derived from the
/// JWT secret when unset (a startup warning says so).
fn data_key() -> [u8; 32] {
    match Config::try_get().and_then(|c| c.data_encryption_key) {
        Some(key) => key,
        None => derive_key(&jwt_secret(), LABEL_DATA_KEY),
    }
}

/// Key for signing unsubscribe links.
pub fn unsubscribe_key() -> [u8; 32] {
    derive_key(&jwt_secret(), LABEL_UNSUBSCRIBE)
}

fn encrypt_with(key: &[u8; 32], plaintext: &[u8]) -> Result<String, ApiError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| internal("invalid key length"))?;
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), plaintext)
        .map_err(|_| internal("encryption failed"))?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(format!("{ENCRYPTION_VERSION}{}", BASE64.encode(blob)))
}

fn decrypt_with(key: &[u8; 32], stored: &str) -> Result<Vec<u8>, ApiError> {
    let encoded = stored
        .strip_prefix(ENCRYPTION_VERSION)
        .ok_or_else(|| internal("unknown encryption format"))?;
    let blob = BASE64
        .decode(encoded)
        .map_err(|_| internal("corrupt encrypted value"))?;
    if blob.len() <= NONCE_LEN {
        return Err(internal("corrupt encrypted value"));
    }
    let (nonce, ciphertext) = blob.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| internal("corrupt encrypted value"))?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| internal("invalid key length"))?;
    cipher
        .decrypt(&Nonce::from(nonce), ciphertext)
        .map_err(|_| internal("decryption failed (wrong key or tampered value)"))
}

/// Encrypts `plaintext` for storage.
pub fn encrypt(plaintext: &[u8]) -> Result<String, ApiError> {
    encrypt_with(&data_key(), plaintext)
}

/// Decrypts a value produced by [`encrypt`].
pub fn decrypt(stored: &str) -> Result<Vec<u8>, ApiError> {
    decrypt_with(&data_key(), stored)
}

fn internal(message: &'static str) -> ApiError {
    error!("{message}");
    ApiError::ServerError(axum::Error::new(message))
}

/// Hex HMAC-SHA256 of `value` with `key`.
pub fn hmac_hex(key: &[u8], value: &[u8]) -> String {
    hex(&hmac_bytes(key, value))
}

pub fn hmac_bytes(key: &[u8], value: &[u8]) -> Vec<u8> {
    // HMAC accepts keys of any length, so `new_from_slice` can't fail.
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(value);
    mac.finalize().into_bytes().to_vec()
}

/// The stored form of a one-time code (verification code, recovery code).
/// `scope` binds the MAC to its use, so a code for one purpose can never
/// be replayed for another even if the plain values collide.
pub fn hash_code(scope: &str, code: &str) -> String {
    hmac_hex(&codes_key(), format!("{scope}:{code}").as_bytes())
}

/// Constant-time equality for secrets.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Constant-time check of `code` against a stored [`hash_code`] value.
pub fn verify_code(scope: &str, code: &str, stored_hash: &str) -> bool {
    constant_time_eq(hash_code(scope, code).as_bytes(), stored_hash.as_bytes())
}

/// A random, URL-safe token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    BASE64URL.encode(buf)
}

/// Random bytes from the operating system's CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    OsRng.fill_bytes(&mut buf);
    buf
}

/// Hex SHA-256, used to store opaque bearer values (challenge tokens) so
/// the table alone can't be used to complete a sign-in.
pub fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_round_trips_and_is_randomized() {
        let key = [3u8; 32];
        let a = encrypt_with(&key, b"JBSWY3DPEHPK3PXP").unwrap();
        let b = encrypt_with(&key, b"JBSWY3DPEHPK3PXP").unwrap();
        assert!(a.starts_with("v1:"));
        assert_ne!(a, b, "a fresh nonce is used every time");
        assert_eq!(decrypt_with(&key, &a).unwrap(), b"JBSWY3DPEHPK3PXP");
    }

    #[test]
    fn decryption_rejects_tampering_wrong_keys_and_unknown_formats() {
        let key = [3u8; 32];
        let stored = encrypt_with(&key, b"secret").unwrap();
        assert!(decrypt_with(&[4u8; 32], &stored).is_err());

        let mut blob = BASE64.decode(stored.trim_start_matches("v1:")).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 1;
        let tampered = format!("v1:{}", BASE64.encode(blob));
        assert!(decrypt_with(&key, &tampered).is_err());

        assert!(decrypt_with(&key, "v2:abcd").is_err());
        assert!(decrypt_with(&key, "v1:").is_err());
    }

    #[test]
    fn code_hashes_are_scoped_and_verified_in_constant_time() {
        let stored = hash_code("email_verification", "123456");
        assert!(verify_code("email_verification", "123456", &stored));
        assert!(!verify_code("email_verification", "123457", &stored));
        assert!(!verify_code("password_reset", "123456", &stored));
        assert_eq!(stored.len(), 64);
    }

    #[test]
    fn derived_keys_depend_on_the_label() {
        assert_ne!(derive_key(b"s", "a"), derive_key(b"s", "b"));
        assert_eq!(derive_key(b"s", "a"), derive_key(b"s", "a"));
    }

    #[test]
    fn tokens_are_url_safe() {
        let token = random_token(32);
        assert_eq!(token.len(), 43);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
        assert_eq!(hex(&[0x0f, 0xa0]), "0fa0");
    }
}
