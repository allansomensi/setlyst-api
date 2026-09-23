//! Reference links attached to songs and setlists (a recording, a backing
//! track, a chart on a cloud drive). See `validations::link` for what is
//! accepted.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// A link as sent by clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkInput {
    /// `https` URL of a supported provider, at most 500 characters.
    pub url: String,
    /// Optional short description (at most 60 characters).
    #[serde(default)]
    pub label: Option<String>,
}

/// Where a link points to. Derived from the URL's host, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkProvider {
    Youtube,
    Spotify,
    GoogleDrive,
    AppleMusic,
    Deezer,
    Soundcloud,
    Dropbox,
    Onedrive,
}

/// A link as stored in the `links` JSONB columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StoredLink {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// A link as returned to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Link {
    pub url: String,
    pub label: Option<String>,
    pub provider: LinkProvider,
}

impl Link {
    /// Re-derives the provider of a stored link. `None` when the URL no
    /// longer qualifies (it is then left out of responses).
    pub fn from_stored(stored: StoredLink) -> Option<Self> {
        let provider = crate::validations::link::provider_of(&stored.url)?;
        Some(Self {
            url: stored.url,
            label: stored.label,
            provider,
        })
    }

    pub fn to_stored(&self) -> StoredLink {
        StoredLink {
            url: self.url.clone(),
            label: self.label.clone(),
        }
    }
}

/// The links of a song or setlist. Decoded straight from the JSONB column
/// (`[{url, label}]`), with each provider computed on the way out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct Links(pub Vec<Link>);

impl<'de> Deserialize<'de> for Links {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = Option::<Vec<StoredLink>>::deserialize(deserializer)?.unwrap_or_default();
        Ok(Links(
            stored.into_iter().filter_map(Link::from_stored).collect(),
        ))
    }
}

impl Links {
    pub fn from_stored(stored: Vec<StoredLink>) -> Self {
        Links(stored.into_iter().filter_map(Link::from_stored).collect())
    }

    pub fn to_stored(&self) -> Vec<StoredLink> {
        self.0.iter().map(Link::to_stored).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_stored_shape_and_serializes_with_providers() {
        let links: Links = serde_json::from_str(
            r#"[{"url":"https://youtu.be/x","label":"Live"},{"url":"https://evil.com"}]"#,
        )
        .unwrap();
        assert_eq!(links.0.len(), 1);
        let json = serde_json::to_value(&links).unwrap();
        assert_eq!(json[0]["provider"], "youtube");
        assert_eq!(json[0]["label"], "Live");
        let empty: Links = serde_json::from_str("null").unwrap();
        assert!(empty.is_empty());
    }
}
