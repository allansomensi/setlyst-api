use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

/// Where a gig currently stands.
#[derive(ToSchema, PartialEq, Eq, Debug, Clone, Copy, Serialize, Deserialize, Type, Default)]
#[serde(rename_all(serialize = "lowercase", deserialize = "lowercase"))]
#[sqlx(type_name = "gig_status", rename_all = "lowercase")]
pub enum GigStatus {
    #[default]
    Confirmed,
    Cancelled,
    Completed,
}

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Gig {
    pub id: Uuid,
    pub user_id: Uuid,
    /// The band this gig belongs to, or `None` for a personal/solo gig.
    pub band_id: Option<Uuid>,
    /// The setlist to be played, or `None` if it hasn't been chosen yet.
    pub setlist_id: Option<Uuid>,
    pub venue: String,
    pub scheduled_at: NaiveDateTime,
    pub status: GigStatus,
    pub notes: Option<String>,
    /// A long, random, unguessable token that resolves this gig through the
    /// public (unauthenticated) `/public/gigs/{token}` endpoints, or `None`
    /// if public sharing isn't enabled. Same convention as
    /// [`crate::models::setlist::Setlist::share_token`].
    pub share_token: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateGigPayload {
    #[validate(length(min = 1, max = 255, message = "Venue must be between 1 and 255 chars."))]
    pub venue: String,
    pub scheduled_at: NaiveDateTime,
    /// Optionally attach the gig to a band instead of keeping it personal.
    /// The caller must be a member of the band with permission to manage
    /// its gigs.
    pub band_id: Option<Uuid>,
    /// The setlist to play, if already decided. Must belong to the same
    /// scope as the gig itself (same band, or the caller's own personal
    /// setlist).
    pub setlist_id: Option<Uuid>,
    pub status: Option<GigStatus>,
    pub notes: Option<String>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateGigPayload {
    #[validate(length(min = 1, max = 255, message = "Venue must be between 1 and 255 chars."))]
    pub venue: Option<String>,
    pub scheduled_at: Option<NaiveDateTime>,
    pub setlist_id: Option<Uuid>,
    pub status: Option<GigStatus>,
    pub notes: Option<String>,
}

impl Gig {
    pub fn new(
        venue: &str,
        scheduled_at: NaiveDateTime,
        user_id: Uuid,
        band_id: Option<Uuid>,
        setlist_id: Option<Uuid>,
        status: GigStatus,
        notes: Option<String>,
    ) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            user_id,
            band_id,
            setlist_id,
            venue: venue.to_string(),
            scheduled_at,
            status,
            notes,
            share_token: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// The read-only shape returned by the public (unauthenticated) gig
/// endpoints. Deliberately excludes internal fields — `id`, `user_id`,
/// `band_id`, `share_token` — that an anonymous viewer has no use for.
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicGig {
    pub venue: String,
    pub scheduled_at: NaiveDateTime,
    pub status: GigStatus,
    pub setlist: Option<crate::models::setlist::PublicSetlist>,
}
