//! Items pinned to the home screen. Purely personal: pinning grants no
//! access and never changes what anyone else sees.

use serde::{Deserialize, Serialize};
use sqlx::prelude::Type;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Most items one account may pin.
pub const MAX_PINS: i64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "pin_item_type", rename_all = "lowercase")]
pub enum PinItemType {
    Setlist,
    Band,
    Song,
    Tour,
    Gig,
}

impl PinItemType {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "setlist" => Some(PinItemType::Setlist),
            "band" => Some(PinItemType::Band),
            "song" => Some(PinItemType::Song),
            "tour" => Some(PinItemType::Tour),
            "gig" => Some(PinItemType::Gig),
            _ => None,
        }
    }
}

/// A pinned item, resolved for display. Items that no longer exist, are in
/// the trash or are no longer accessible are silently left out.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PinnedItem {
    pub item_type: PinItemType,
    pub item_id: Uuid,
    pub position: i32,
    pub title: String,
    /// Song: artist. Setlist/tour: band name (if any). Gig: `YYYY-MM-DD
    /// HH:MM`. Band: `null`.
    pub subtitle: Option<String>,
    pub band_id: Option<Uuid>,
    /// `true` for a band's repertoire (a setlist whose `title` is the
    /// stored, untranslated "Repertoire"); `false` for anything else.
    pub is_repertoire: bool,
    /// Suggested dashboard path (`/dashboard/setlists/{id}`...).
    pub href_hint: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
pub struct PinRef {
    pub item_type: PinItemType,
    pub item_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReorderPinsPayload {
    /// The pins in their new order. Pins left out keep their relative
    /// order after the listed ones.
    #[validate(length(max = 12, message = "At most 12 pins."))]
    pub items: Vec<PinRef>,
}

/// Implemented by responses that carry the caller's `is_pinned` flag.
pub trait Pinnable {
    const PIN_TYPE: PinItemType;
    fn pin_id(&self) -> Uuid;
    fn set_pinned(&mut self, pinned: bool);
}

impl Pinnable for crate::models::song::Song {
    const PIN_TYPE: PinItemType = PinItemType::Song;
    fn pin_id(&self) -> Uuid {
        self.id
    }
    fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
    }
}

impl Pinnable for crate::models::setlist::Setlist {
    const PIN_TYPE: PinItemType = PinItemType::Setlist;
    fn pin_id(&self) -> Uuid {
        self.id
    }
    fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
    }
}

impl Pinnable for crate::models::gig::Gig {
    const PIN_TYPE: PinItemType = PinItemType::Gig;
    fn pin_id(&self) -> Uuid {
        self.id
    }
    fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
    }
}

impl Pinnable for crate::models::tour::Tour {
    const PIN_TYPE: PinItemType = PinItemType::Tour;
    fn pin_id(&self) -> Uuid {
        self.id
    }
    fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
    }
}

impl Pinnable for crate::models::band::BandWithMembership {
    const PIN_TYPE: PinItemType = PinItemType::Band;
    fn pin_id(&self) -> Uuid {
        self.id
    }
    fn set_pinned(&mut self, pinned: bool) {
        self.is_pinned = pinned;
    }
}
