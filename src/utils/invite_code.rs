use argon2::password_hash::rand_core::{OsRng, RngCore};

/// Alphabet chosen to avoid visually ambiguous characters (0/O, 1/I/l)
/// since invite codes are meant to be read aloud and typed by hand.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// 16 characters over a 31-symbol alphabet is ~79 bits of entropy: far
/// beyond online guessing, even with the accept endpoint's rate limit off.
pub const CODE_LENGTH: usize = 16;

/// Generates a human-friendly, random invite code (e.g.
/// `"K7P4Q2XMH3RWT9ZA"`). Bytes are drawn with rejection sampling, so every
/// symbol is equally likely (no modulo bias).
pub fn generate_invite_code() -> String {
    // Largest multiple of the alphabet size that fits in a byte.
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    let mut code = String::with_capacity(CODE_LENGTH);
    let mut buffer = [0u8; 32];
    while code.len() < CODE_LENGTH {
        OsRng.fill_bytes(&mut buffer);
        for byte in buffer {
            if byte < limit && code.len() < CODE_LENGTH {
                code.push(ALPHABET[(byte as usize) % ALPHABET.len()] as char);
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_long_and_use_the_unambiguous_alphabet() {
        let code = generate_invite_code();
        assert_eq!(code.len(), CODE_LENGTH);
        assert!(code.bytes().all(|b| ALPHABET.contains(&b)));
        assert_ne!(generate_invite_code(), code);
    }
}
