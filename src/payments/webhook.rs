//! Stripe webhook signatures and event envelopes.
//!
//! Verification follows Stripe's manual procedure: the `Stripe-Signature`
//! header carries `t=<unix time>` and one or more `v1=<hex HMAC-SHA256>`
//! signatures of `"<t>.<raw body>"` keyed with the endpoint secret. Only
//! the `v1` scheme counts (anything else is ignored to prevent downgrade
//! attacks), comparison is constant-time, and a timestamp outside the
//! tolerance is rejected so a captured request can't be replayed.

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Largest accepted clock difference, in seconds (Stripe's default).
pub const TOLERANCE_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// Header missing or without a timestamp / `v1` signature.
    Malformed,
    /// No `v1` signature matches.
    Mismatch,
    /// Signed too long ago (or in the future).
    Expired,
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(value.get(i..i + 2)?, 16).ok())
        .collect()
}

fn expected_signature(payload: &[u8], timestamp: &str, secret: &str) -> Vec<u8> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

/// Checks `header` (the `Stripe-Signature` value) against the raw request
/// body. `now` is the current Unix time.
pub fn verify_signature(
    payload: &[u8],
    header: &str,
    secret: &str,
    now: i64,
) -> Result<(), SignatureError> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = Some(value),
            Some(("v1", value)) => signatures.push(value),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    let signed_at: i64 = timestamp.parse().map_err(|_| SignatureError::Malformed)?;
    if signatures.is_empty() {
        return Err(SignatureError::Malformed);
    }

    let expected = expected_signature(payload, timestamp, secret);
    let matches = signatures.iter().any(|candidate| {
        decode_hex(candidate)
            .is_some_and(|candidate| bool::from(candidate.as_slice().ct_eq(&expected)))
    });
    if !matches {
        return Err(SignatureError::Mismatch);
    }
    if (now - signed_at).abs() > TOLERANCE_SECS {
        return Err(SignatureError::Expired);
    }
    Ok(())
}

/// Builds a valid `Stripe-Signature` header (tests and local tooling).
pub fn sign(payload: &[u8], secret: &str, timestamp: i64) -> String {
    let signature: String = expected_signature(payload, &timestamp.to_string(), secret)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("t={timestamp},v1={signature}")
}

/// The parts of an event the API acts on. Everything else about the
/// subscription is fetched fresh from the API, so the event's own API
/// version and delivery order don't matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeEvent {
    pub id: String,
    pub kind: String,
    /// The subscription the event concerns, if any.
    pub subscription_id: Option<String>,
    /// `client_reference_id` of a completed checkout (the account id).
    pub client_reference_id: Option<String>,
    /// `metadata.redemption_id` of a completed checkout.
    pub redemption_id: Option<String>,
    /// Whether the event comes from live mode (`None` when absent).
    pub livemode: Option<bool>,
    /// Id of the event's object (checkout session, customer, dispute...).
    pub object_id: Option<String>,
    /// A completed checkout whose buyer ticked the required Terms of
    /// Service box.
    pub terms_accepted: bool,
    /// `metadata.terms_version` of a completed checkout.
    pub terms_version: Option<String>,
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Parses an event body. `None` for anything that isn't an event.
pub fn parse_event(body: &Value) -> Option<StripeEvent> {
    if body.get("object").and_then(Value::as_str) != Some("event") {
        return None;
    }
    let id = string_at(body, "/id")?;
    let kind = string_at(body, "/type")?;
    let object = body.pointer("/data/object")?;
    let object_type = object.get("object").and_then(Value::as_str).unwrap_or("");

    let checkout = object_type == "checkout.session";
    let (subscription_id, client_reference_id, redemption_id) = match object_type {
        "subscription" => (string_at(object, "/id"), None, None),
        "checkout.session"
            if object.get("mode").and_then(Value::as_str) == Some("subscription") =>
        {
            (
                string_at(object, "/subscription"),
                string_at(object, "/client_reference_id"),
                string_at(object, "/metadata/redemption_id"),
            )
        }
        // Invoices name their subscription in different places depending
        // on the event's API version.
        "invoice" => (
            string_at(object, "/parent/subscription_details/subscription")
                .or_else(|| string_at(object, "/subscription")),
            None,
            None,
        ),
        _ => (None, None, None),
    };

    Some(StripeEvent {
        id,
        kind,
        subscription_id,
        client_reference_id,
        redemption_id,
        livemode: body.get("livemode").and_then(Value::as_bool),
        object_id: string_at(object, "/id"),
        terms_accepted: checkout
            && object
                .pointer("/consent/terms_of_service")
                .and_then(Value::as_str)
                == Some("accepted"),
        terms_version: checkout
            .then(|| string_at(object, "/metadata/terms_version"))
            .flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "whsec_test_secret";

    #[test]
    fn a_signed_payload_verifies() {
        let body = br#"{"id":"evt_1"}"#;
        let header = sign(body, SECRET, 1_000);
        assert_eq!(verify_signature(body, &header, SECRET, 1_000), Ok(()));
        assert_eq!(verify_signature(body, &header, SECRET, 1_299), Ok(()));
    }

    #[test]
    fn tampering_wrong_secrets_and_replays_are_rejected() {
        let body = br#"{"id":"evt_1"}"#;
        let header = sign(body, SECRET, 1_000);
        assert_eq!(
            verify_signature(br#"{"id":"evt_2"}"#, &header, SECRET, 1_000),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_signature(body, &header, "whsec_other", 1_000),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_signature(body, &header, SECRET, 1_000 + TOLERANCE_SECS + 1),
            Err(SignatureError::Expired)
        );
        assert_eq!(
            verify_signature(body, &header, SECRET, 1_000 - TOLERANCE_SECS - 1),
            Err(SignatureError::Expired)
        );
    }

    #[test]
    fn only_v1_signatures_count_and_any_of_several_may_match() {
        let body = br#"{"id":"evt_1"}"#;
        let good = sign(body, SECRET, 1_000);
        let v1 = good.split_once(",v1=").unwrap().1;
        // A rolled secret: one stale signature and the current one.
        let rolled = format!("t=1000,v1={},v1={v1}", "ab".repeat(32));
        assert_eq!(verify_signature(body, &rolled, SECRET, 1_000), Ok(()));
        // The right signature under another scheme is not accepted.
        let downgraded = format!("t=1000,v0={v1}");
        assert_eq!(
            verify_signature(body, &downgraded, SECRET, 1_000),
            Err(SignatureError::Malformed)
        );
    }

    #[test]
    fn malformed_headers_are_rejected() {
        let body = b"{}";
        for header in ["", "t=abc,v1=00", "v1=00", "t=1000", "t=1000,v1=zz"] {
            assert_ne!(
                verify_signature(body, header, SECRET, 1_000),
                Ok(()),
                "{header}"
            );
        }
    }

    #[test]
    fn events_point_at_their_subscription() {
        let subscription = json!({
            "object": "event", "id": "evt_1", "type": "customer.subscription.updated",
            "data": {"object": {"object": "subscription", "id": "sub_1"}}
        });
        let event = parse_event(&subscription).unwrap();
        assert_eq!(event.subscription_id.as_deref(), Some("sub_1"));

        assert_eq!(event.livemode, None);
        assert!(!event.terms_accepted);

        let checkout = json!({
            "object": "event", "id": "evt_2", "type": "checkout.session.completed",
            "livemode": true,
            "data": {"object": {
                "object": "checkout.session", "id": "cs_2", "mode": "subscription",
                "subscription": "sub_2", "client_reference_id": "3f1c",
                "metadata": {"redemption_id": "r1", "terms_version": "2026-09-01"},
                "consent": {"terms_of_service": "accepted"}
            }}
        });
        let event = parse_event(&checkout).unwrap();
        assert_eq!(event.subscription_id.as_deref(), Some("sub_2"));
        assert_eq!(event.client_reference_id.as_deref(), Some("3f1c"));
        assert_eq!(event.redemption_id.as_deref(), Some("r1"));
        assert_eq!(event.livemode, Some(true));
        assert_eq!(event.object_id.as_deref(), Some("cs_2"));
        assert!(event.terms_accepted);
        assert_eq!(event.terms_version.as_deref(), Some("2026-09-01"));

        let one_off = json!({
            "object": "event", "id": "evt_3", "type": "checkout.session.completed",
            "data": {"object": {"object": "checkout.session", "mode": "payment"}}
        });
        assert_eq!(parse_event(&one_off).unwrap().subscription_id, None);

        let invoice = json!({
            "object": "event", "id": "evt_4", "type": "invoice.payment_failed",
            "data": {"object": {"object": "invoice",
                "parent": {"subscription_details": {"subscription": "sub_4"}}}}
        });
        assert_eq!(
            parse_event(&invoice).unwrap().subscription_id.as_deref(),
            Some("sub_4")
        );

        assert!(parse_event(&json!({"object": "list"})).is_none());
    }
}
