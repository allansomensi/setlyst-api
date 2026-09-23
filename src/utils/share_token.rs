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

/// A short, non-reversible fingerprint of a share token (the first 12 hex
/// digits of its SHA-256), for logs: enough to correlate requests, useless
/// to open the shared page.
pub fn token_fingerprint(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_short_and_stable() {
        let token = generate_share_token();
        assert_eq!(token.len(), 64);
        let print = token_fingerprint(&token);
        assert_eq!(print.len(), 12);
        assert_eq!(print, token_fingerprint(&token));
        assert!(!token.contains(&print));
    }
}
