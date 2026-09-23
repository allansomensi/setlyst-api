//! Human-facing one-time codes and identifiers.
//!
//! Everything here is drawn from the operating system's CSPRNG. Codes a
//! person types (recovery codes, referral codes) use an alphabet without
//! look-alike characters (no 0/O, 1/I/L).

use rand::{Rng, rngs::OsRng};

/// Letters and digits that can't be confused with each other when read
/// aloud or copied by hand.
const UNAMBIGUOUS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// Number of recovery codes issued when two-factor authentication is
/// enabled (or the codes are regenerated).
pub const RECOVERY_CODE_COUNT: usize = 10;

/// A 6-digit numeric code (e-mail verification, password recovery),
/// uniformly distributed, zero-padded.
pub fn numeric_code() -> String {
    format!("{:06}", OsRng.gen_range(0..1_000_000u32))
}

fn unambiguous(len: usize) -> String {
    (0..len)
        .map(|_| UNAMBIGUOUS[OsRng.gen_range(0..UNAMBIGUOUS.len())] as char)
        .collect()
}

/// A recovery code shaped `XXXX-XXXX` (~39 bits of entropy each).
pub fn recovery_code() -> String {
    format!("{}-{}", unambiguous(4), unambiguous(4))
}

/// Normalizes a typed recovery code: upper case, no spaces, the dash
/// re-inserted, so "abcd efgh", "ABCDEFGH" and "abcd-efgh" all match.
pub fn normalize_recovery_code(input: &str) -> Option<String> {
    let compact: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (compact.len() == 8).then(|| format!("{}-{}", &compact[..4], &compact[4..]))
}

/// A referral code (10 characters from the unambiguous alphabet).
pub fn referral_code() -> String {
    unambiguous(10)
}

/// A random suffix for generated usernames (`user-k3j9x2`).
pub fn lowercase_suffix(len: usize) -> String {
    unambiguous(len).to_ascii_lowercase()
}

/// A random number in `lo..hi`.
pub fn random_in(lo: u32, hi: u32) -> u32 {
    OsRng.gen_range(lo..hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_codes_are_six_digits() {
        for _ in 0..200 {
            let code = numeric_code();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn recovery_codes_use_the_unambiguous_alphabet() {
        for _ in 0..100 {
            let code = recovery_code();
            assert_eq!(code.len(), 9);
            assert_eq!(&code[4..5], "-");
            assert!(
                code.chars()
                    .filter(|c| *c != '-')
                    .all(|c| UNAMBIGUOUS.contains(&(c as u8)))
            );
        }
    }

    #[test]
    fn recovery_codes_are_normalized() {
        assert_eq!(
            normalize_recovery_code(" abcd efgh "),
            Some("ABCD-EFGH".to_string())
        );
        assert_eq!(
            normalize_recovery_code("ABCD-EFGH"),
            Some("ABCD-EFGH".to_string())
        );
        assert_eq!(normalize_recovery_code("ABC"), None);
    }

    #[test]
    fn referral_codes_are_ten_characters() {
        let code = referral_code();
        assert_eq!(code.len(), 10);
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
