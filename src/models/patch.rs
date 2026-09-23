//! Helpers for PATCH payloads with clearable fields.
//!
//! A plain `Option<T>` can't tell "field absent" (leave unchanged) apart
//! from "field is `null`" (clear it), so every nullable column used to be
//! impossible to clear once set — removing a song's BPM in the UI simply
//! did nothing. Fields that can be cleared use `Option<Option<T>>` with
//! [`double_option`]:
//!
//! - absent  → `None`            (unchanged)
//! - `null`  → `Some(None)`      (clear)
//! - value   → `Some(Some(v))`   (set)

use serde::{Deserialize, Deserializer};

pub fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Payload {
        #[serde(default, deserialize_with = "double_option")]
        tempo: Option<Option<i32>>,
    }

    #[test]
    fn distinguishes_absent_null_and_value() {
        let absent: Payload = serde_json::from_str("{}").unwrap();
        let null: Payload = serde_json::from_str(r#"{"tempo": null}"#).unwrap();
        let value: Payload = serde_json::from_str(r#"{"tempo": 120}"#).unwrap();
        assert_eq!(absent.tempo, None);
        assert_eq!(null.tempo, Some(None));
        assert_eq!(value.tempo, Some(Some(120)));
    }
}
