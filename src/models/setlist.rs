use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

use super::{
    link::{LinkInput, Links},
    song::PublicSong,
};

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Setlist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub user_id: Uuid,
    /// The band this setlist belongs to, or `None` for a personal setlist.
    pub band_id: Option<Uuid>,
    /// A long, random, unguessable token that resolves this setlist through
    /// the public (unauthenticated) `/public/setlists/{token}` endpoints, or
    /// `None` if public sharing isn't enabled. See [`crate::utils::share_token::generate_share_token`].
    pub share_token: Option<String>,
    pub total_duration: i32,
    /// Whether the *caller* has favorited this setlist — personal, never
    /// affects anyone else's view or any permission. `false` on the
    /// public (unauthenticated) share endpoints, which have no caller.
    pub is_favorite: bool,
    /// Set when staff took the public link down; the owner can't re-share
    /// until it's unlocked.
    #[sqlx(default)]
    pub share_locked_at: Option<NaiveDateTime>,
    #[sqlx(default)]
    pub share_lock_reason: Option<String>,
    /// Number of songs in the running order (blocks/breaks excluded).
    #[sqlx(default)]
    pub song_count: i64,
    /// Username of the creator (`user_id`).
    #[sqlx(default)]
    pub owner_username: Option<String>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    /// Reference links (a playlist, the charts folder...).
    #[sqlx(json, default)]
    pub links: Links,
    /// `true` for a band's repertoire: the special setlist that collects
    /// every song the band plays. It can't be deleted or renamed.
    #[sqlx(default)]
    pub is_repertoire: bool,
    /// Whether the *caller* pinned this setlist to their home screen.
    #[sqlx(default)]
    pub is_pinned: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// `POST /setlists/{id}/duplicate` answer: the new personal setlist plus
/// how many band songs could not be copied into the caller's library
/// (their song or artist quota was reached).
#[derive(Debug, Serialize, ToSchema)]
pub struct DuplicateSetlistResponse {
    #[serde(flatten)]
    pub setlist: Setlist,
    pub skipped_band_songs: i64,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub description: Option<String>,
    /// Optionally create the setlist under a band instead of personally.
    /// The caller must be a member of the band with permission to manage its setlists.
    pub band_id: Option<Uuid>,
    /// At most 5 links to supported providers.
    #[validate(length(max = 5, message = "At most 5 links are allowed."))]
    pub links: Option<Vec<LinkInput>>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    /// Absent = unchanged, `null` = clear.
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub description: Option<Option<String>>,
    /// Absent = unchanged, `[]` = remove every link.
    #[validate(length(max = 5, message = "At most 5 links are allowed."))]
    pub links: Option<Vec<LinkInput>>,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct AddSongToSetlistPayload {
    pub song_id: Uuid,
}

/// Optional title override when duplicating a setlist. When omitted, the
/// backend derives one from the original title (see
/// `SetlistRepository::duplicate`).
#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct DuplicateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
}

/// A non-song entry in a setlist's running order: either a named
/// block/section header, or a break/pause slot.
#[derive(ToSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "setlist_marker_type", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum SetlistMarkerType {
    Block,
    Break,
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SetlistMarker {
    pub id: Uuid,
    pub setlist_id: Uuid,
    pub marker_type: SetlistMarkerType,
    /// Block name (for `block` markers) or an optional break label (for
    /// `break` markers — the frontend falls back to a translated
    /// placeholder such as "Break" when this is `None`).
    pub label: Option<String>,
    /// Only meaningful for `break` markers.
    pub duration_minutes: Option<i32>,
    pub position: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistBlockPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Block name must be between 1 and 255 chars."
    ))]
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistBlockPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Block name must be between 1 and 255 chars."
    ))]
    pub name: String,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistBreakPayload {
    #[validate(length(max = 255, message = "Label must be at most 255 chars."))]
    pub label: Option<String>,
    #[validate(range(
        min = 0,
        max = 1440,
        message = "Duration must be a realistic number of minutes."
    ))]
    pub duration_minutes: Option<i32>,
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistBreakPayload {
    #[validate(length(max = 255, message = "Label must be at most 255 chars."))]
    pub label: Option<String>,
    #[validate(range(
        min = 0,
        max = 1440,
        message = "Duration must be a realistic number of minutes."
    ))]
    pub duration_minutes: Option<i32>,
}

/// One entry in the combined, position-ordered view of a setlist's
/// contents — a song, a block header, or a break — used by
/// `GET /setlists/{id}/items` and the reorder endpoint that follows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "item_type", rename_all = "lowercase")]
pub enum SetlistItem {
    Song {
        position: i32,
        song: Box<crate::models::song::SongWithArtist>,
    },
    Block {
        position: i32,
        id: Uuid,
        name: String,
    },
    Break {
        position: i32,
        id: Uuid,
        label: Option<String>,
        duration_minutes: Option<i32>,
    },
}

/// A reference to one item in a setlist's timeline, used to describe the
/// desired order in `ReorderSetlistItemsPayload`. `item_type` says which
/// table `id` belongs to: `song` -> `songs.id`, `block`/`break` ->
/// `setlist_markers.id`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SetlistItemType {
    Song,
    Block,
    Break,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct SetlistItemRef {
    pub item_type: SetlistItemType,
    pub id: Uuid,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReorderSetlistItemsPayload {
    #[validate(length(
        min = 1,
        max = 1000,
        message = "The list of items must have between 1 and 1000 entries."
    ))]
    pub items: Vec<SetlistItemRef>,
}

impl Setlist {
    pub fn new(
        title: &str,
        description: Option<String>,
        user_id: Uuid,
        band_id: Option<Uuid>,
    ) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            title: title.to_string(),
            description,
            user_id,
            band_id,
            share_token: None,
            total_duration: 0,
            is_favorite: false,
            share_locked_at: None,
            share_lock_reason: None,
            song_count: 0,
            owner_username: None,
            updated_by: None,
            updated_by_username: None,
            links: Links::default(),
            is_repertoire: false,
            is_pinned: false,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReorderSetlistSongsPayload {
    #[validate(length(
        min = 1,
        max = 1000,
        message = "The list of song IDs must have between 1 and 1000 entries."
    ))]
    pub song_ids: Vec<Uuid>,
}

/// A block header or break on the public share endpoints (no ids or
/// timestamps).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicMarker {
    pub marker_type: SetlistMarkerType,
    pub label: Option<String>,
    pub duration_minutes: Option<i32>,
    pub position: i32,
}

impl From<SetlistMarker> for PublicMarker {
    fn from(marker: SetlistMarker) -> Self {
        Self {
            marker_type: marker.marker_type,
            label: marker.label,
            duration_minutes: marker.duration_minutes,
            position: marker.position,
        }
    }
}

/// The read-only shape returned by the public (unauthenticated) setlist
/// endpoints. Only what a viewer needs to follow the running order: no
/// account, band or record identifiers, usernames, tags or timestamps.
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicSetlist {
    pub title: String,
    pub description: Option<String>,
    pub total_duration: i32,
    pub links: Links,
    pub songs: Vec<PublicSong>,
    /// Block headers and breaks in the setlist's running order. Merge these
    /// with `songs` by `position` to reconstruct the full timeline.
    pub markers: Vec<PublicMarker>,
}
