use argon2::password_hash::rand_core::{OsRng, RngCore};

/// Generates a cryptographically random, URL-safe token for public setlist
/// share links: 32 bytes (256 bits) of entropy, hex-encoded. That's the
/// entire security model for the public share routes — there's no
/// authentication on them, so the token must be infeasible to guess or
/// enumerate.
pub fn generate_share_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
