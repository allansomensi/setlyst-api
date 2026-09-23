//! Time-based one-time passwords (RFC 6238) on top of HOTP (RFC 4226).
//!
//! Parameters are the ones every authenticator app understands without
//! configuration: HMAC-SHA1, 6 digits, 30-second steps. Verification
//! accepts the previous and next step as well (±30 s of clock drift) and
//! reports *which* step matched, so the caller can refuse a code whose
//! step was already used (replay protection).

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const DIGITS: u32 = 6;
pub const STEP_SECONDS: u64 = 30;
/// Steps accepted on each side of the current one.
pub const WINDOW: i64 = 1;
/// Length of generated secrets (160 bits, as recommended by RFC 4226).
pub const SECRET_LEN: usize = 20;

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding (what `otpauth://` URIs use).
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            let index = (buffer >> (bits - 5)) & 0x1f;
            out.push(BASE32_ALPHABET[index as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        let index = (buffer << (5 - bits)) & 0x1f;
        out.push(BASE32_ALPHABET[index as usize] as char);
    }
    out
}

/// Decodes base32, ignoring case, spaces and padding. `None` on any other
/// character.
pub fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in input.chars() {
        if c == '=' || c == ' ' || c == '-' {
            continue;
        }
        let upper = c.to_ascii_uppercase() as u8;
        let value = BASE32_ALPHABET.iter().position(|&a| a == upper)? as u32;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
        buffer &= (1 << bits) - 1;
    }
    Some(out)
}

/// HOTP value (RFC 4226 §5.3) with `digits` digits.
pub fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[hash.len() - 1] & 0x0f) as usize;
    let binary = ((hash[offset] as u32 & 0x7f) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | (hash[offset + 3] as u32);
    binary % 10u32.pow(digits)
}

/// The time step containing `unix_seconds`.
pub fn step_at(unix_seconds: u64) -> u64 {
    unix_seconds / STEP_SECONDS
}

/// The code for `unix_seconds`, zero-padded.
pub fn code_at(secret: &[u8], unix_seconds: u64) -> String {
    format!(
        "{:0width$}",
        hotp(secret, step_at(unix_seconds), DIGITS),
        width = DIGITS as usize
    )
}

/// Checks `code` against the steps around `unix_seconds` and returns the
/// matching step. Steps at or before `last_used_step` are refused, so a
/// code can be used only once.
pub fn verify(
    secret: &[u8],
    code: &str,
    unix_seconds: u64,
    last_used_step: Option<i64>,
) -> Option<i64> {
    let code = code.trim().replace(' ', "");
    if code.len() != DIGITS as usize || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let current = step_at(unix_seconds) as i64;
    let mut matched = None;
    // Every candidate is computed (no early exit) so timing doesn't reveal
    // which step matched.
    for delta in -WINDOW..=WINDOW {
        let step = current + delta;
        if step < 0 {
            continue;
        }
        let candidate = format!(
            "{:0width$}",
            hotp(secret, step as u64, DIGITS),
            width = DIGITS as usize
        );
        if crate::utils::crypto::constant_time_eq(candidate.as_bytes(), code.as_bytes())
            && last_used_step.is_none_or(|last| step > last)
            && matched.is_none()
        {
            matched = Some(step);
        }
    }
    matched
}

/// The `otpauth://` URI authenticator apps scan as a QR code.
pub fn otpauth_url(issuer: &str, account: &str, secret_b32: &str) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret_b32}&issuer={issuer}&digits={DIGITS}&period={STEP_SECONDS}",
        issuer = percent_encode(issuer),
        account = percent_encode(account),
    )
}

/// Percent-encodes everything outside the RFC 3986 unreserved set.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4226 Appendix D: secret "12345678901234567890", counters 0..9.
    #[test]
    fn hotp_matches_rfc_4226_vectors() {
        let secret = b"12345678901234567890";
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (counter, value) in expected.iter().enumerate() {
            assert_eq!(hotp(secret, counter as u64, 6), *value, "counter {counter}");
        }
    }

    /// RFC 6238 Appendix B (SHA1 column). The RFC lists 8-digit values;
    /// a 6-digit code is the same number modulo 10^6.
    #[test]
    fn totp_matches_rfc_6238_vectors() {
        let secret = b"12345678901234567890";
        let vectors: [(u64, u32); 6] = [
            (59, 94287082),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ];
        for (time, expected) in vectors {
            assert_eq!(hotp(secret, step_at(time), 8), expected, "time {time}");
            assert_eq!(
                code_at(secret, time),
                format!("{:06}", expected % 1_000_000)
            );
        }
    }

    #[test]
    fn verification_accepts_one_step_of_drift_and_refuses_replays() {
        let secret = b"12345678901234567890";
        let now = 1_234_567_890;
        let code = code_at(secret, now);
        let step = step_at(now) as i64;

        assert_eq!(verify(secret, &code, now, None), Some(step));
        assert_eq!(verify(secret, &code, now + 30, None), Some(step));
        assert_eq!(verify(secret, &code, now - 30, None), Some(step));
        assert_eq!(verify(secret, &code, now + 90, None), None);
        // Already used: refused.
        assert_eq!(verify(secret, &code, now, Some(step)), None);
        assert_eq!(verify(secret, "12345", now, None), None);
        assert_eq!(verify(secret, "abcdef", now, None), None);
    }

    #[test]
    fn base32_round_trips_and_matches_rfc_4648() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw 6ytb oi======").unwrap(), b"foobar");
        assert!(base32_decode("M1").is_none());
        let secret = [0xABu8; SECRET_LEN];
        assert_eq!(base32_decode(&base32_encode(&secret)).unwrap(), secret);
    }

    #[test]
    fn otpauth_url_is_well_formed() {
        let url = otpauth_url("Setlyst", "ana.maria", "ABC");
        assert_eq!(
            url,
            "otpauth://totp/Setlyst:ana.maria?secret=ABC&issuer=Setlyst&digits=6&period=30"
        );
        assert_eq!(percent_encode("a b@c"), "a%20b%40c");
    }
}
