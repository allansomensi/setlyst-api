use crate::models::{band::BandRole, user::Role};
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
    fn new(user_id: Uuid, notification_type: NotificationType, data: Value) -> Self {
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

/// Response for `GET /notifications/unread-count`.
#[derive(ToSchema, Debug, Clone, Serialize, Deserialize)]
pub struct UnreadCountResponse {
    pub unread_count: i64,
}
