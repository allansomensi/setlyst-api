//! One-click unsubscribe tokens.
//!
//! A token is `base64url(user_id ‖ category ‖ mac)`: the 16 bytes of the
//! user id, one byte identifying the category and the first 16 bytes of an
//! HMAC-SHA256 over both, keyed with a secret derived from the server key.
//! Tokens don't expire (links in old e-mails must keep working) and can
//! only ever switch one category off for one user, so there is nothing
//! more to protect.

use crate::{
    models::communication::Category,
    utils::crypto::{constant_time_eq, hmac_bytes, unsubscribe_key},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL};
use uuid::Uuid;

const MAC_LEN: usize = 16;

fn mac(user_id: Uuid, category: Category) -> Vec<u8> {
    let mut message = user_id.as_bytes().to_vec();
    message.push(category.index());
    let mut full = hmac_bytes(&unsubscribe_key(), &message);
    full.truncate(MAC_LEN);
    full
}

/// The token for switching `category` e-mails off for `user_id`.
pub fn token(user_id: Uuid, category: Category) -> String {
    let mut bytes = user_id.as_bytes().to_vec();
    bytes.push(category.index());
    bytes.extend(mac(user_id, category));
    BASE64URL.encode(bytes)
}

/// Verifies a token. `None` when malformed, forged or for a category that
/// can't be unsubscribed from.
pub fn parse(token: &str) -> Option<(Uuid, Category)> {
    let bytes = BASE64URL.decode(token.trim()).ok()?;
    if bytes.len() != 16 + 1 + MAC_LEN {
        return None;
    }
    let user_id = Uuid::from_slice(&bytes[..16]).ok()?;
    let category = Category::from_index(bytes[16])?;
    if category.locked() || !constant_time_eq(&bytes[17..], &mac(user_id, category)) {
        return None;
    }
    Some((user_id, category))
}

/// The link placed in e-mails: `{base}/{locale}/unsubscribe?token=...`.
pub fn link(app_base_url: &str, locale: &str, user_id: Uuid, category: Category) -> String {
    format!(
        "{}/{}/unsubscribe?token={}",
        app_base_url.trim_end_matches('/'),
        locale,
        token(user_id, category)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_and_reject_tampering() {
        let user = Uuid::new_v4();
        let token = token(user, Category::Bands);
        assert_eq!(parse(&token), Some((user, Category::Bands)));

        let mut bytes = BASE64URL.decode(&token).unwrap();
        bytes[16] = Category::Marketing.index();
        assert_eq!(parse(&BASE64URL.encode(&bytes)), None);

        let mut bytes = BASE64URL.decode(&token).unwrap();
        bytes[0] ^= 1;
        assert_eq!(parse(&BASE64URL.encode(&bytes)), None);

        assert_eq!(parse("garbage"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn security_can_never_be_unsubscribed() {
        let user = Uuid::new_v4();
        assert_eq!(parse(&token(user, Category::Security)), None);
    }
}
