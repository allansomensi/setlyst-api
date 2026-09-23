//! Announcements published by staff: shown as a modal, a banner, an
//! in-app notification and/or an e-mail to a targeted audience.

use crate::models::{patch::double_option, user_preferences::SUPPORTED_LANGUAGES};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

pub const AUDIENCE_ROLES: [&str; 3] = ["user", "moderator", "admin"];
/// Pseudo plan codes of the audience filter: no live subscription, and a
/// running trial.
pub const AUDIENCE_PLAN_NONE: &str = "none";
pub const AUDIENCE_PLAN_TRIAL: &str = "trial";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "announcement_level", rename_all = "snake_case")]
pub enum AnnouncementLevel {
    Info,
    Success,
    Warning,
    /// A service notice: e-mailed to every targeted verified address,
    /// regardless of preferences.
    Critical,
}

impl AnnouncementLevel {
    pub fn key(&self) -> &'static str {
        match self {
            AnnouncementLevel::Info => "info",
            AnnouncementLevel::Success => "success",
            AnnouncementLevel::Warning => "warning",
            AnnouncementLevel::Critical => "critical",
        }
    }
}

/// Lifecycle of an announcement, computed from its timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncementStatus {
    Draft,
    Scheduled,
    Active,
    Ended,
    Archived,
}

impl AnnouncementStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "draft" => Some(Self::Draft),
            "scheduled" => Some(Self::Scheduled),
            "active" => Some(Self::Active),
            "ended" => Some(Self::Ended),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct Announcement {
    pub id: Uuid,
    pub title: String,
    /// Plain text; line breaks are meaningful, nothing is rendered as HTML.
    pub body: String,
    pub level: AnnouncementLevel,
    pub show_modal: bool,
    pub show_banner: bool,
    pub send_notification: bool,
    pub send_email: bool,
    pub dismissible: bool,
    pub requires_acknowledgement: bool,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    /// `None` = everybody (for each dimension).
    pub audience_roles: Option<Vec<String>>,
    pub audience_plans: Option<Vec<String>>,
    pub audience_locales: Option<Vec<String>>,
    pub starts_at: Option<NaiveDateTime>,
    pub ends_at: Option<NaiveDateTime>,
    pub published_at: Option<NaiveDateTime>,
    pub archived_at: Option<NaiveDateTime>,
    pub delivered_at: Option<NaiveDateTime>,
    pub created_by_username: Option<String>,
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    /// Computed; not stored.
    #[sqlx(skip)]
    pub status: Option<AnnouncementStatus>,
}

impl Announcement {
    pub fn compute_status(&self, now: NaiveDateTime) -> AnnouncementStatus {
        if self.archived_at.is_some() {
            AnnouncementStatus::Archived
        } else if self.published_at.is_none() {
            AnnouncementStatus::Draft
        } else if self.starts_at.is_some_and(|s| s > now) {
            AnnouncementStatus::Scheduled
        } else if self.ends_at.is_some_and(|e| e <= now) {
            AnnouncementStatus::Ended
        } else {
            AnnouncementStatus::Active
        }
    }

    /// Fills in `status`.
    pub fn with_status(mut self, now: NaiveDateTime) -> Self {
        self.status = Some(self.compute_status(now));
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AnnouncementStats {
    /// Accounts matching the audience right now.
    pub targeted: i64,
    pub seen: i64,
    pub dismissed: i64,
    pub acknowledged: i64,
}

/// An announcement in the staff console.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminAnnouncement {
    #[serde(flatten)]
    pub announcement: Announcement,
    pub stats: AnnouncementStats,
}

#[derive(Debug, Clone, Default, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AnnouncementReceipt {
    pub seen_at: Option<NaiveDateTime>,
    pub dismissed_at: Option<NaiveDateTime>,
    pub acknowledged_at: Option<NaiveDateTime>,
}

/// An announcement as seen by its recipient.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserAnnouncement {
    #[serde(flatten)]
    pub announcement: Announcement,
    pub receipt: AnnouncementReceipt,
}

/// `GET /announcements/active`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActiveAnnouncements {
    /// To show as modals, oldest first.
    pub modal: Vec<UserAnnouncement>,
    pub banner: Vec<UserAnnouncement>,
}

/// Who an announcement is for. `None` (or an empty list) = no restriction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, Validate)]
pub struct AudiencePayload {
    #[validate(length(max = 3))]
    pub audience_roles: Option<Vec<String>>,
    #[validate(length(max = 20))]
    pub audience_plans: Option<Vec<String>>,
    #[validate(length(max = 3))]
    pub audience_locales: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AudiencePreview {
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct CreateAnnouncementPayload {
    pub title: String,
    pub body: String,
    pub level: Option<AnnouncementLevel>,
    #[serde(default)]
    pub show_modal: bool,
    #[serde(default)]
    pub show_banner: bool,
    pub send_notification: Option<bool>,
    #[serde(default)]
    pub send_email: bool,
    pub dismissible: Option<bool>,
    #[serde(default)]
    pub requires_acknowledgement: bool,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    pub audience_roles: Option<Vec<String>>,
    pub audience_plans: Option<Vec<String>>,
    pub audience_locales: Option<Vec<String>>,
    pub starts_at: Option<NaiveDateTime>,
    pub ends_at: Option<NaiveDateTime>,
}

/// `PATCH /admin/announcements/{id}`. Nullable fields accept `null` to
/// clear them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, Validate)]
pub struct UpdateAnnouncementPayload {
    pub title: Option<String>,
    pub body: Option<String>,
    pub level: Option<AnnouncementLevel>,
    pub show_modal: Option<bool>,
    pub show_banner: Option<bool>,
    pub send_notification: Option<bool>,
    pub send_email: Option<bool>,
    pub dismissible: Option<bool>,
    pub requires_acknowledgement: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>)]
    pub cta_label: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>)]
    pub cta_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<Vec<String>>)]
    pub audience_roles: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<Vec<String>>)]
    pub audience_plans: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<Vec<String>>)]
    pub audience_locales: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<NaiveDateTime>)]
    pub starts_at: Option<Option<NaiveDateTime>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<NaiveDateTime>)]
    pub ends_at: Option<Option<NaiveDateTime>>,
}

impl UpdateAnnouncementPayload {
    /// `true` when only fields editable on a live announcement are set
    /// (text, call to action, end of the window and display flags).
    pub fn only_live_editable_fields(&self) -> bool {
        self.level.is_none()
            && self.send_notification.is_none()
            && self.send_email.is_none()
            && self.dismissible.is_none()
            && self.requires_acknowledgement.is_none()
            && self.audience_roles.is_none()
            && self.audience_plans.is_none()
            && self.audience_locales.is_none()
            && self.starts_at.is_none()
    }
}

/// The complete editable state of an announcement, checked as a whole
/// (create, or after applying a PATCH).
#[derive(Debug, Clone)]
pub struct AnnouncementDraft {
    pub title: String,
    pub body: String,
    pub level: AnnouncementLevel,
    pub show_modal: bool,
    pub show_banner: bool,
    pub send_notification: bool,
    pub send_email: bool,
    pub dismissible: bool,
    pub requires_acknowledgement: bool,
    pub cta_label: Option<String>,
    pub cta_url: Option<String>,
    pub audience_roles: Option<Vec<String>>,
    pub audience_plans: Option<Vec<String>>,
    pub audience_locales: Option<Vec<String>>,
    pub starts_at: Option<NaiveDateTime>,
    pub ends_at: Option<NaiveDateTime>,
}

/// Empty lists mean "no restriction"; entries are trimmed and de-duplicated.
pub fn normalize_audience(list: Option<Vec<String>>) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for item in list? {
        let item = item.trim().to_string();
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A call-to-action link: an app path (`/dashboard/...`, not `//host`) or
/// an `https` URL, at most 500 characters.
pub fn is_valid_cta_url(url: &str) -> bool {
    if url.chars().count() > 500 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Some(rest) = url.strip_prefix('/') {
        return !rest.starts_with('/') && !rest.starts_with('\\');
    }
    match reqwest::Url::parse(url) {
        Ok(parsed) => {
            parsed.scheme() == "https"
                && parsed.domain().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
        }
        Err(_) => false,
    }
}

impl AnnouncementDraft {
    /// Checks every rule except plan codes (which need the database).
    /// Returns the first problem as a message.
    pub fn check(&self) -> Result<(), String> {
        let title_len = self.title.trim().chars().count();
        if !(3..=120).contains(&title_len) {
            return Err("The title must have between 3 and 120 characters.".into());
        }
        let body_len = self.body.trim().chars().count();
        if !(1..=5000).contains(&body_len) {
            return Err("The text must have between 1 and 5000 characters.".into());
        }
        match (&self.cta_label, &self.cta_url) {
            (Some(label), Some(url)) => {
                let len = label.trim().chars().count();
                if !(1..=40).contains(&len) {
                    return Err("The button label must have between 1 and 40 characters.".into());
                }
                if !is_valid_cta_url(url) {
                    return Err(
                        "The button link must be an app path starting with '/' or an https URL."
                            .into(),
                    );
                }
            }
            (None, None) => {}
            _ => return Err("A button needs both a label and a link.".into()),
        }
        if let Some(roles) = &self.audience_roles
            && roles.iter().any(|r| !AUDIENCE_ROLES.contains(&r.as_str()))
        {
            return Err("Unknown role in the audience.".into());
        }
        if let Some(locales) = &self.audience_locales
            && locales
                .iter()
                .any(|l| !SUPPORTED_LANGUAGES.contains(&l.as_str()))
        {
            return Err("Unknown language in the audience.".into());
        }
        if let (Some(start), Some(end)) = (self.starts_at, self.ends_at)
            && end <= start
        {
            return Err("The end must be after the start.".into());
        }
        if !(self.show_modal || self.show_banner || self.send_notification || self.send_email) {
            return Err("Choose at least one channel.".into());
        }
        if self.requires_acknowledgement && !self.show_modal {
            return Err(
                "Announcements that require acknowledgement must be shown as a modal.".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AnnouncementListQuery {
    /// `draft`, `scheduled`, `active`, `ended` or `archived`.
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn draft() -> AnnouncementDraft {
        AnnouncementDraft {
            title: "Maintenance".into(),
            body: "We will be back soon.".into(),
            level: AnnouncementLevel::Info,
            show_modal: false,
            show_banner: true,
            send_notification: false,
            send_email: false,
            dismissible: true,
            requires_acknowledgement: false,
            cta_label: None,
            cta_url: None,
            audience_roles: None,
            audience_plans: None,
            audience_locales: None,
            starts_at: None,
            ends_at: None,
        }
    }

    #[test]
    fn drafts_are_checked_as_a_whole() {
        assert!(draft().check().is_ok());
        let mut d = draft();
        d.title = "Hi".into();
        assert!(d.check().is_err());
        let mut d = draft();
        d.show_banner = false;
        assert!(d.check().is_err(), "no channel");
        let mut d = draft();
        d.requires_acknowledgement = true;
        assert!(d.check().is_err(), "ack without modal");
        let mut d = draft();
        d.cta_label = Some("Open".into());
        assert!(d.check().is_err(), "label without url");
        d.cta_url = Some("/dashboard/settings".into());
        assert!(d.check().is_ok());
        d.cta_url = Some("//evil.example".into());
        assert!(d.check().is_err());
        d.cta_url = Some("javascript:alert(1)".into());
        assert!(d.check().is_err());
        d.cta_url = Some("https://setlyst.app/pricing".into());
        assert!(d.check().is_ok());
        let mut d = draft();
        d.audience_roles = Some(vec!["root".into()]);
        assert!(d.check().is_err());
        let mut d = draft();
        let now = Utc::now().naive_utc();
        d.starts_at = Some(now);
        d.ends_at = Some(now - Duration::hours(1));
        assert!(d.check().is_err());
    }

    #[test]
    fn audience_lists_are_normalized() {
        assert_eq!(normalize_audience(Some(vec![])), None);
        assert_eq!(
            normalize_audience(Some(vec![" en ".into(), "en".into(), "es".into()])),
            Some(vec!["en".to_string(), "es".to_string()])
        );
    }
}
