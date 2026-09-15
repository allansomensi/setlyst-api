use argon2::password_hash::rand_core::{OsRng, RngCore};

/// Alphabet chosen to avoid visually ambiguous characters (0/O, 1/I/l)
/// since invite codes are meant to be read aloud and typed by hand.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 8;

/// Generates a short, human-friendly, random invite code (e.g. `"K7P4Q2XM"`).
pub fn generate_invite_code() -> String {
    let mut bytes = [0u8; CODE_LENGTH];
    OsRng.fill_bytes(&mut bytes);

    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}
