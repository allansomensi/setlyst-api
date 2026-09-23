use crate::models::{band::BandRole, communication::Category, user::Role};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;

/// The kind of system event a [`Notification`] reports. Every variant is
/// generated internally by the API in response to a state change made by
/// someone other than the recipient — notifications are never
/// user-authored.
#[derive(ToSchema, PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(rename_all(serialize = "snake_case", deserialize = "snake_case"))]
#[sqlx(type_name = "notification_type", rename_all = "snake_case")]
pub enum NotificationType {
    /// The recipient's role within a band changed (promotion or demotion).
    BandRoleChanged,
    /// The recipient was removed from a band by another member.
    BandMemberRemoved,
    /// The recipient's site-wide role changed (admin/moderator/user).
    PlatformRoleChanged,
    /// The recipient was added to a band directly by platform staff.
    BandMemberAdded,
    /// Staff took down the public link of one of the recipient's setlists
    /// or gigs.
    ShareLinkRevoked,
    /// A platform announcement targeted at the recipient.
    Announcement,
    /// New release notes were published.
    ReleasePublished,
    /// A band member suggested a song for one of the recipient's bands.
    BandSuggestionCreated,
    /// A song suggestion made by the recipient was accepted, rejected or
    /// withdrawn.
    BandSuggestionResolved,
    /// Staff acted on the recipient's profile (avatar removed, username
    /// reset...).
    ModerationAction,
    /// The recipient's plan or subscription status changed.
    SubscriptionChanged,
    /// The recipient's trial ends in a few days.
    TrialEnding,
    /// Credits were added to the recipient's balance.
    CreditsGranted,
    /// A security-relevant change on the account (2FA, e-mail, password).
    SecurityAlert,
}

impl NotificationType {
    /// The communication category that governs how this notification is
    /// delivered (see `models::communication`).
    pub fn category(&self) -> Category {
        match self {
            NotificationType::BandRoleChanged
            | NotificationType::BandMemberRemoved
            | NotificationType::BandMemberAdded
            | NotificationType::BandSuggestionCreated
            | NotificationType::BandSuggestionResolved => Category::Bands,
            NotificationType::PlatformRoleChanged
            | NotificationType::ShareLinkRevoked
            | NotificationType::ModerationAction
            | NotificationType::SubscriptionChanged
            | NotificationType::TrialEnding
            | NotificationType::CreditsGranted => Category::Account,
            NotificationType::Announcement => Category::Announcements,
            NotificationType::ReleasePublished => Category::ProductUpdates,
            NotificationType::SecurityAlert => Category::Security,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            NotificationType::BandRoleChanged => "band_role_changed",
            NotificationType::BandMemberRemoved => "band_member_removed",
            NotificationType::PlatformRoleChanged => "platform_role_changed",
            NotificationType::BandMemberAdded => "band_member_added",
            NotificationType::ShareLinkRevoked => "share_link_revoked",
            NotificationType::Announcement => "announcement",
            NotificationType::ReleasePublished => "release_published",
            NotificationType::BandSuggestionCreated => "band_suggestion_created",
            NotificationType::BandSuggestionResolved => "band_suggestion_resolved",
            NotificationType::ModerationAction => "moderation_action",
            NotificationType::SubscriptionChanged => "subscription_changed",
            NotificationType::TrialEnding => "trial_ending",
            NotificationType::CreditsGranted => "credits_granted",
            NotificationType::SecurityAlert => "security_alert",
        }
    }
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
    /// Structured context for rendering the notification (band name, old
    /// and new roles, who made the change, etc). Left untyped on purpose —
    /// its shape depends on `notification_type`, and it exists purely for
    /// the frontend to interpolate into a translated message.
    pub data: Value,
    pub read_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

impl Notification {
    pub fn new(user_id: Uuid, notification_type: NotificationType, data: Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            user_id,
            notification_type,
            data,
            read_at: None,
            created_at: Utc::now().naive_utc(),
        }
    }

    pub fn band_role_changed(
        user_id: Uuid,
        band_id: Uuid,
        band_name: &str,
        old_role: BandRole,
        new_role: BandRole,
        actor_id: Uuid,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::BandRoleChanged,
            json!({
                "band_id": band_id,
                "band_name": band_name,
                "old_role": old_role,
                "new_role": new_role,
                "actor_id": actor_id,
            }),
        )
    }

    pub fn band_member_removed(
        user_id: Uuid,
        band_id: Uuid,
        band_name: &str,
        actor_id: Uuid,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::BandMemberRemoved,
            json!({
                "band_id": band_id,
                "band_name": band_name,
                "actor_id": actor_id,
            }),
        )
    }

    pub fn platform_role_changed(
        user_id: Uuid,
        old_role: Role,
        new_role: Role,
        actor_id: Uuid,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::PlatformRoleChanged,
            json!({
                "old_role": old_role,
                "new_role": new_role,
                "actor_id": actor_id,
            }),
        )
    }
}

impl Notification {
    pub fn band_member_added(
        user_id: Uuid,
        band_id: Uuid,
        band_name: &str,
        role: BandRole,
        actor_id: Uuid,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::BandMemberAdded,
            json!({
                "band_id": band_id,
                "band_name": band_name,
                "role": role,
                "actor_id": actor_id,
            }),
        )
    }

    /// `kind` is `"setlist"` or `"gig"`.
    pub fn share_link_revoked(
        user_id: Uuid,
        kind: &str,
        target_id: Uuid,
        title: &str,
        reason: Option<&str>,
        actor_id: Uuid,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::ShareLinkRevoked,
            json!({
                "kind": kind,
                "target_id": target_id,
                "title": title,
                "reason": reason,
                "actor_id": actor_id,
            }),
        )
    }
}

impl Notification {
    /// Staff acted on the recipient's profile. `action` is the resolution
    /// (`avatar_removed`, `band_logo_removed`, `username_reset`).
    pub fn moderation_action(
        user_id: Uuid,
        action: &str,
        note: Option<&str>,
        band: Option<(Uuid, &str)>,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::ModerationAction,
            json!({
                "action": action,
                "note": note,
                "band_id": band.map(|(id, _)| id),
                "band_name": band.map(|(_, name)| name),
            }),
        )
    }

    /// `kind` is what happened (`plan_granted`, `trial_started`,
    /// `expired`, `revoked`...).
    pub fn subscription_changed(
        user_id: Uuid,
        kind: &str,
        plan_code: Option<&str>,
        status: Option<&str>,
        current_period_end: Option<NaiveDateTime>,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::SubscriptionChanged,
            json!({
                "kind": kind,
                "plan_code": plan_code,
                "status": status,
                "current_period_end": current_period_end,
            }),
        )
    }

    pub fn trial_ending(user_id: Uuid, plan_code: &str, ends_at: NaiveDateTime) -> Self {
        Self::new(
            user_id,
            NotificationType::TrialEnding,
            json!({ "plan_code": plan_code, "ends_at": ends_at }),
        )
    }

    /// `reason` is the ledger reason (`referral_referrer`, `promo_code`...).
    pub fn credits_granted(user_id: Uuid, amount: i32, reason: &str) -> Self {
        Self::new(
            user_id,
            NotificationType::CreditsGranted,
            json!({ "amount": amount, "reason": reason }),
        )
    }

    /// `event` is `two_factor_enabled`, `two_factor_disabled`,
    /// `email_changed`, `password_changed` or `recovery_codes_regenerated`.
    pub fn security_alert(user_id: Uuid, event: &str) -> Self {
        Self::new(
            user_id,
            NotificationType::SecurityAlert,
            json!({ "event": event }),
        )
    }

    pub fn release_published(user_id: Uuid, version: &str, release_id: Uuid) -> Self {
        Self::new(
            user_id,
            NotificationType::ReleasePublished,
            json!({ "version": version, "release_id": release_id }),
        )
    }
}

/// Response for `GET /notifications/unread-count`.
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct UnreadCountResponse {
    pub unread_count: i64,
}

impl Notification {
    /// A band member suggested a song (sent to the other members).
    /// `suggested_by` is the suggester's username.
    pub fn band_suggestion_created(
        user_id: Uuid,
        band_id: Uuid,
        band_name: &str,
        suggestion_id: Uuid,
        song_title: &str,
        suggested_by: &str,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::BandSuggestionCreated,
            json!({
                "band_id": band_id,
                "band_name": band_name,
                "suggestion_id": suggestion_id,
                "song_title": song_title,
                "suggested_by": suggested_by,
            }),
        )
    }

    /// The recipient's suggestion was closed. `status` is `accepted`,
    /// `rejected` or `withdrawn`.
    pub fn band_suggestion_resolved(
        user_id: Uuid,
        band_id: Uuid,
        band_name: &str,
        suggestion_id: Uuid,
        song_title: &str,
        status: &str,
    ) -> Self {
        Self::new(
            user_id,
            NotificationType::BandSuggestionResolved,
            json!({
                "band_id": band_id,
                "band_name": band_name,
                "suggestion_id": suggestion_id,
                "song_title": song_title,
                "status": status,
            }),
        )
    }
}
