//! Communication preferences: which categories of messages reach a user
//! by e-mail and in the app.
//!
//! Stored in `user_preferences.communication` as
//! `{"categories": {"bands": {"email": false, "in_app": true}, ...}}`.
//! Missing categories or flags fall back to [`Category::default_prefs`],
//! so adding a category later needs no data migration. Security messages
//! can't be turned off.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// A family of messages the user can opt in or out of.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Sign-in and account protection notices. Always on.
    Security,
    /// Changes to the account itself (roles, plan, credits, moderation).
    Account,
    /// Band activity (roles, members, suggestions).
    Bands,
    /// Announcements published by the Setlyst team.
    Announcements,
    /// Release notes ("What's new").
    ProductUpdates,
    /// Offers and news. Opt-in only.
    Marketing,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Security,
        Category::Account,
        Category::Bands,
        Category::Announcements,
        Category::ProductUpdates,
        Category::Marketing,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Category::Security => "security",
            Category::Account => "account",
            Category::Bands => "bands",
            Category::Announcements => "announcements",
            Category::ProductUpdates => "product_updates",
            Category::Marketing => "marketing",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }

    /// Stable one-byte identifier used inside unsubscribe tokens.
    pub fn index(&self) -> u8 {
        Self::ALL.iter().position(|c| c == self).unwrap_or(0) as u8
    }

    pub fn from_index(index: u8) -> Option<Self> {
        Self::ALL.get(index as usize).copied()
    }

    /// Security messages can never be switched off.
    pub fn locked(&self) -> bool {
        matches!(self, Category::Security)
    }

    pub fn default_prefs(&self) -> ChannelPrefs {
        let (email, in_app) = match self {
            Category::Security => (true, true),
            Category::Account => (true, true),
            Category::Bands => (false, true),
            Category::Announcements => (true, true),
            Category::ProductUpdates => (false, true),
            Category::Marketing => (false, false),
        };
        ChannelPrefs { email, in_app }
    }
}

/// Delivery switches for one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelPrefs {
    pub email: bool,
    pub in_app: bool,
}

/// One category as returned to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CategoryPrefs {
    pub email: bool,
    pub in_app: bool,
    /// `true` when the user can't change it (security).
    pub locked: bool,
}

/// The effective preferences of a user (stored values over defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunicationPreferences {
    pub categories: BTreeMap<Category, ChannelPrefs>,
}

impl Default for CommunicationPreferences {
    fn default() -> Self {
        Self {
            categories: Category::ALL
                .into_iter()
                .map(|c| (c, c.default_prefs()))
                .collect(),
        }
    }
}

impl CommunicationPreferences {
    /// Reads the stored JSON, tolerating anything malformed (it falls back
    /// to the defaults field by field).
    pub fn from_stored(value: &Value) -> Self {
        let mut prefs = Self::default();
        let Some(categories) = value.get("categories").and_then(Value::as_object) else {
            return prefs;
        };
        for (key, stored) in categories {
            let Some(category) = Category::from_key(key) else {
                continue;
            };
            if category.locked() {
                continue;
            }
            let entry = prefs
                .categories
                .entry(category)
                .or_insert(category.default_prefs());
            if let Some(email) = stored.get("email").and_then(Value::as_bool) {
                entry.email = email;
            }
            if let Some(in_app) = stored.get("in_app").and_then(Value::as_bool) {
                entry.in_app = in_app;
            }
        }
        prefs
    }

    pub fn get(&self, category: Category) -> ChannelPrefs {
        if category.locked() {
            return category.default_prefs();
        }
        self.categories
            .get(&category)
            .copied()
            .unwrap_or(category.default_prefs())
    }

    pub fn set(&mut self, category: Category, prefs: ChannelPrefs) {
        if !category.locked() {
            self.categories.insert(category, prefs);
        }
    }

    /// The JSON persisted in `user_preferences.communication`.
    pub fn to_stored(&self) -> Value {
        let mut categories = Map::new();
        for category in Category::ALL {
            if category.locked() {
                continue;
            }
            let prefs = self.get(category);
            categories.insert(
                category.key().to_string(),
                json!({ "email": prefs.email, "in_app": prefs.in_app }),
            );
        }
        json!({ "categories": categories })
    }

    pub fn to_view(&self) -> BTreeMap<String, CategoryPrefs> {
        Category::ALL
            .into_iter()
            .map(|c| {
                let prefs = self.get(c);
                (
                    c.key().to_string(),
                    CategoryPrefs {
                        email: prefs.email,
                        in_app: prefs.in_app,
                        locked: c.locked(),
                    },
                )
            })
            .collect()
    }
}

/// `GET /users/me/communication`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CommunicationSettings {
    /// Keyed by category: `security`, `account`, `bands`, `announcements`,
    /// `product_updates`, `marketing`.
    pub categories: BTreeMap<String, CategoryPrefs>,
    /// E-mails are only sent to a verified address.
    pub email_verified: bool,
    pub email: Option<String>,
}

/// `PUT /users/me/communication`: the categories to change. Omitted
/// categories keep their value; `security` is ignored.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateCommunicationPayload {
    pub categories: BTreeMap<String, ChannelPrefs>,
}

/// Body of `POST /public/email/unsubscribe`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, validator::Validate)]
pub struct UnsubscribePayload {
    #[validate(length(min = 10, max = 200))]
    pub token: String,
}

/// Query of `GET /public/email/unsubscribe`.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UnsubscribeQuery {
    pub token: String,
}

/// What an unsubscribe token refers to.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UnsubscribeInfo {
    /// The category (`null` when the token is invalid).
    pub category: Option<Category>,
    pub valid: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let prefs = CommunicationPreferences::default();
        assert_eq!(
            prefs.get(Category::Bands),
            ChannelPrefs {
                email: false,
                in_app: true
            }
        );
        assert_eq!(
            prefs.get(Category::Marketing),
            ChannelPrefs {
                email: false,
                in_app: false
            }
        );
        assert!(prefs.get(Category::Security).email);
    }

    #[test]
    fn stored_values_override_defaults_but_never_security() {
        let stored = json!({ "categories": {
            "bands": { "email": true },
            "security": { "email": false, "in_app": false },
            "unknown": { "email": true },
            "marketing": "garbage"
        }});
        let prefs = CommunicationPreferences::from_stored(&stored);
        assert_eq!(
            prefs.get(Category::Bands),
            ChannelPrefs {
                email: true,
                in_app: true
            }
        );
        assert!(prefs.get(Category::Security).email);
        assert!(prefs.get(Category::Security).in_app);
        assert!(!prefs.get(Category::Marketing).email);

        let round = CommunicationPreferences::from_stored(&prefs.to_stored());
        assert_eq!(round, prefs);
        assert!(prefs.to_stored()["categories"].get("security").is_none());
    }

    #[test]
    fn category_indexes_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::from_index(c.index()), Some(c));
            assert_eq!(Category::from_key(c.key()), Some(c));
        }
        assert!(Category::from_index(99).is_none());
    }
}
