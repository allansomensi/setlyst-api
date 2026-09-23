//! Moderation flags on profile images, usernames and band logos.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Type};
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

use crate::models::user::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "moderation_target", rename_all = "snake_case")]
pub enum ModerationTarget {
    Avatar,
    Username,
    BandLogo,
    /// The profile as a whole (reports about impersonation, spam...).
    Profile,
}

impl ModerationTarget {
    pub fn key(&self) -> &'static str {
        match self {
            ModerationTarget::Avatar => "avatar",
            ModerationTarget::Username => "username",
            ModerationTarget::BandLogo => "band_logo",
            ModerationTarget::Profile => "profile",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "moderation_status", rename_all = "snake_case")]
pub enum ModerationStatus {
    Open,
    Dismissed,
    Actioned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "moderation_source", rename_all = "snake_case")]
pub enum ModerationSource {
    Automatic,
    Report,
}

/// A flag about to be stored.
#[derive(Debug, Clone)]
pub struct NewFlag {
    pub target_type: ModerationTarget,
    pub user_id: Uuid,
    pub band_id: Option<Uuid>,
    pub value: String,
    pub reasons: Vec<String>,
    pub score: Option<f32>,
    pub details: Value,
    pub source: ModerationSource,
    pub reported_by: Option<Uuid>,
    pub report_note: Option<String>,
}

/// The account a flag is about.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FlagUser {
    pub id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
    pub role: Role,
    pub is_banned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FlagBand {
    pub id: Uuid,
    pub name: String,
}

/// One entry of the moderation queue.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModerationFlag {
    pub id: Uuid,
    pub target_type: ModerationTarget,
    pub user: FlagUser,
    pub band: Option<FlagBand>,
    /// The flagged value when the flag was raised.
    pub value: String,
    /// The current avatar, username or logo, to see whether it changed
    /// since.
    pub current_value: Option<String>,
    pub reasons: Vec<String>,
    pub score: Option<f32>,
    pub details: Value,
    pub source: ModerationSource,
    pub reported_by_username: Option<String>,
    pub report_note: Option<String>,
    pub status: ModerationStatus,
    pub resolution: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_by_username: Option<String>,
    pub resolved_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

/// Flat row behind [`ModerationFlag`].
#[derive(Debug, Clone, FromRow)]
pub struct ModerationFlagRow {
    pub id: Uuid,
    pub target_type: ModerationTarget,
    pub user_id: Uuid,
    pub username: String,
    pub user_avatar_url: Option<String>,
    pub user_role: Role,
    pub user_is_banned: bool,
    pub band_id: Option<Uuid>,
    pub band_name: Option<String>,
    pub band_logo_url: Option<String>,
    pub value: String,
    pub reasons: Vec<String>,
    pub score: Option<f32>,
    pub details: Value,
    pub source: ModerationSource,
    pub reported_by_username: Option<String>,
    pub report_note: Option<String>,
    pub status: ModerationStatus,
    pub resolution: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_by_username: Option<String>,
    pub resolved_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
}

impl From<ModerationFlagRow> for ModerationFlag {
    fn from(row: ModerationFlagRow) -> Self {
        let current_value = match row.target_type {
            ModerationTarget::Avatar => row.user_avatar_url.clone(),
            ModerationTarget::Username | ModerationTarget::Profile => Some(row.username.clone()),
            ModerationTarget::BandLogo => row.band_logo_url.clone(),
        };
        let band = match (row.band_id, row.band_name) {
            (Some(id), Some(name)) => Some(FlagBand { id, name }),
            _ => None,
        };
        Self {
            id: row.id,
            target_type: row.target_type,
            user: FlagUser {
                id: row.user_id,
                username: row.username,
                avatar_url: row.user_avatar_url,
                role: row.user_role,
                is_banned: row.user_is_banned,
            },
            band,
            value: row.value,
            current_value,
            reasons: row.reasons,
            score: row.score,
            details: row.details,
            source: row.source,
            reported_by_username: row.reported_by_username,
            report_note: row.report_note,
            status: row.status,
            resolution: row.resolution,
            resolution_note: row.resolution_note,
            resolved_by_username: row.resolved_by_username,
            resolved_at: row.resolved_at,
            created_at: row.created_at,
        }
    }
}

/// Query string of `GET /admin/moderation/flags`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FlagListQuery {
    /// `open` (default), `dismissed`, `actioned` or `all`.
    pub status: Option<String>,
    pub target_type: Option<ModerationTarget>,
    /// Only flags about this account.
    pub user_id: Option<Uuid>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// `GET /admin/moderation/summary`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModerationSummary {
    pub open_total: i64,
    /// Open flags per target type.
    pub by_type: BTreeMap<String, i64>,
}

/// What a moderator does with a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolveAction {
    /// Nothing wrong: close the flag.
    Dismiss,
    RemoveAvatar,
    RemoveBandLogo,
    /// Replace the username with a random `user-xxxxxx` one the owner can
    /// change right away.
    ResetUsername,
}

impl ResolveAction {
    /// The `resolution` stored on the flag.
    pub fn resolution(&self) -> &'static str {
        match self {
            ResolveAction::Dismiss => "dismissed",
            ResolveAction::RemoveAvatar => "avatar_removed",
            ResolveAction::RemoveBandLogo => "band_logo_removed",
            ResolveAction::ResetUsername => "username_reset",
        }
    }
}

/// Body of `POST /admin/moderation/flags/{id}/resolve`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct ResolveFlagPayload {
    pub action: ResolveAction,
    #[validate(length(max = 500))]
    pub note: Option<String>,
    /// Send the owner a `moderation_action` notification.
    #[serde(default)]
    pub notify_user: bool,
}

/// `POST /admin/moderation/rescan`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RescanResponse {
    /// New flags raised by the scan.
    pub flagged: i64,
}
