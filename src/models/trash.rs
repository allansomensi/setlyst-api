//! The trash: deleted songs, artists, setlists, gigs and tours wait here
//! (hidden everywhere else) until they are restored or purged.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Kinds of trashable content. Also the `{type}` path segment of the
/// trash endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrashType {
    Song,
    Artist,
    Setlist,
    Gig,
    Tour,
}

impl TrashType {
    pub fn key(&self) -> &'static str {
        match self {
            TrashType::Song => "song",
            TrashType::Artist => "artist",
            TrashType::Setlist => "setlist",
            TrashType::Gig => "gig",
            TrashType::Tour => "tour",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "song" => Some(TrashType::Song),
            "artist" => Some(TrashType::Artist),
            "setlist" => Some(TrashType::Setlist),
            "gig" => Some(TrashType::Gig),
            "tour" => Some(TrashType::Tour),
            _ => None,
        }
    }

    /// Band permission needed to see and manage this kind in a band's
    /// trash: songs and artists follow `manage_songs`, the rest
    /// `manage_setlists`.
    pub fn band_permission(&self) -> crate::models::band::BandPermission {
        match self {
            TrashType::Song | TrashType::Artist => crate::models::band::BandPermission::ManageSongs,
            _ => crate::models::band::BandPermission::ManageSetlists,
        }
    }
}

/// Whose trash.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TrashScope {
    /// The caller's personal content.
    #[default]
    Personal,
    /// A band's content (`band_id` required).
    Band,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrashListQuery {
    pub scope: Option<TrashScope>,
    pub band_id: Option<Uuid>,
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub item_type: Option<TrashType>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EmptyTrashQuery {
    pub scope: Option<TrashScope>,
    pub band_id: Option<Uuid>,
}

/// One entry of the trash.
#[derive(Debug, Clone, Serialize, ToSchema, FromRow)]
pub struct TrashItem {
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    #[schema(value_type = TrashType)]
    pub item_type: String,
    pub id: Uuid,
    /// Song title, artist name, setlist title, gig venue or tour name.
    pub title: String,
    /// Song: its artist. Gig: `YYYY-MM-DD HH:MM` (+ `, location`). Tour:
    /// `YYYY-MM-DD..YYYY-MM-DD`. Setlist and artist: the band name (band
    /// trash) or `null`.
    pub subtitle: Option<String>,
    pub band_id: Option<Uuid>,
    pub band_name: Option<String>,
    pub deleted_at: NaiveDateTime,
    pub deleted_by_username: Option<String>,
    /// When it will be deleted for good.
    pub purge_at: NaiveDateTime,
    /// Artists: how many of their songs were deleted with them (restored
    /// together). `0` for everything else.
    pub batch_count: i64,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EmptyTrashResponse {
    pub deleted: u64,
}
