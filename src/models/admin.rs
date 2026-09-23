//! Shapes used by the staff (admin/moderator) console.

use crate::models::{
    band::{BandMember, BandRole},
    quota::{QuotaReport, UserQuotaSettings},
    setlist::SetlistItem,
    song::{Genre, SongWithArtist, Tonality},
    user::UserPublic,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

/// Filters shared by the staff listings. Every field is optional.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Case-insensitive search over names/titles and owner usernames.
    pub q: Option<String>,
    /// Only records created by this user.
    pub user_id: Option<Uuid>,
    /// Only records belonging to this band.
    pub band_id: Option<Uuid>,
    /// Only records whose public link is currently active (`true`) or not.
    pub shared: Option<bool>,
}

impl AdminListQuery {
    pub fn page(&self) -> (i64, i64) {
        crate::models::resolve_page(self.page, self.per_page, 25)
    }

    /// `q` as an escaped `ILIKE` pattern, or `None` when blank.
    pub fn search_pattern(&self) -> Option<String> {
        self.q
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| {
                format!(
                    "%{}%",
                    q.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                )
            })
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AdminBandSummary {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub logo_url: Option<String>,
    pub members_can_manage_setlists: bool,
    pub created_by: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub owner_username: Option<String>,
    pub member_count: i64,
    pub setlist_count: i64,
    pub song_count: i64,
    pub gig_count: i64,
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminBandDetail {
    pub band: AdminBandSummary,
    pub members: Vec<BandMember>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AdminSongSummary {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub artist_name: String,
    pub user_id: Uuid,
    pub owner_username: Option<String>,
    pub band_id: Option<Uuid>,
    pub band_name: Option<String>,
    pub tonality: Option<Tonality>,
    pub tempo: Option<i32>,
    pub genre: Option<Genre>,
    pub duration: Option<i32>,
    #[sqlx(default)]
    pub energy: Option<i16>,
    #[sqlx(default)]
    pub time_signature: Option<String>,
    #[sqlx(default)]
    pub capo: Option<i16>,
    pub has_lyrics: bool,
    pub tags: Vec<String>,
    pub setlist_count: i64,
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// A song as the staff console shows it: the full record (with lyrics)
/// plus the owner, band and usage from the listing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminSongDetail {
    pub song: SongWithArtist,
    pub summary: AdminSongSummary,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AdminSetlistSummary {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub user_id: Uuid,
    pub owner_username: Option<String>,
    pub band_id: Option<Uuid>,
    pub band_name: Option<String>,
    pub song_count: i64,
    pub total_duration: i32,
    pub share_token: Option<String>,
    pub share_locked_at: Option<NaiveDateTime>,
    pub share_lock_reason: Option<String>,
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AdminSetlistDetail {
    pub setlist: AdminSetlistSummary,
    pub items: Vec<SetlistItem>,
}

/// A setlist or gig whose public link is active or was taken down.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SharedLink {
    /// `"setlist"` or `"gig"`.
    pub kind: String,
    pub id: Uuid,
    pub title: String,
    pub owner_id: Uuid,
    pub owner_username: Option<String>,
    pub band_id: Option<Uuid>,
    pub band_name: Option<String>,
    pub share_token: Option<String>,
    pub share_locked_at: Option<NaiveDateTime>,
    pub share_lock_reason: Option<String>,
    pub share_locked_by_username: Option<String>,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct AdminUserBand {
    pub band_id: Uuid,
    pub band_name: String,
    pub role: BandRole,
    pub joined_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminUserOverview {
    pub user: UserPublic,
    pub usage: QuotaReport,
    pub quota_settings: UserQuotaSettings,
    pub bands: Vec<AdminUserBand>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct RevokeSharePayload {
    /// Shown to the owner in their notification and on the item.
    #[validate(custom(function = "crate::validations::text::validate_reason"))]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct AdminUpdateBandMemberRolePayload {
    pub role: BandRole,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct AdminTransferOwnershipPayload {
    pub new_owner_id: Uuid,
}
