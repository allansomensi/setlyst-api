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
    /// Free-text location (address, city, or a maps link) — independent
    /// of `venue`, which is just the venue's name.
    pub location: Option<String>,
    pub scheduled_at: NaiveDateTime,
    pub status: GigStatus,
    pub notes: Option<String>,
    /// A long, random, unguessable token that resolves this gig through the
    /// public (unauthenticated) `/public/gigs/{token}` endpoints, or `None`
    /// if public sharing isn't enabled. Same convention as
    /// [`crate::models::setlist::Setlist::share_token`].
    pub share_token: Option<String>,
    #[sqlx(default)]
    pub share_locked_at: Option<NaiveDateTime>,
    #[sqlx(default)]
    pub share_lock_reason: Option<String>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Clone, Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateGigPayload {
    #[validate(length(min = 1, max = 255, message = "Venue must be between 1 and 255 chars."))]
    pub venue: String,
    #[validate(length(max = 500, message = "Location must be at most 500 chars."))]
    pub location: Option<String>,
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
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub notes: Option<String>,
}

/// Nullable fields use `Option<Option<T>>`: absent = unchanged, `null` =
/// clear (see [`crate::models::patch`]).
#[derive(Deserialize, Serialize, ToSchema, Validate, Default)]
pub struct UpdateGigPayload {
    #[validate(length(min = 1, max = 255, message = "Venue must be between 1 and 255 chars."))]
    pub venue: Option<String>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(length(max = 500, message = "Location must be at most 500 chars."))]
    pub location: Option<Option<String>>,
    pub scheduled_at: Option<NaiveDateTime>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    pub setlist_id: Option<Option<Uuid>>,
    pub status: Option<GigStatus>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[validate(custom(function = "crate::validations::text::validate_description"))]
    pub notes: Option<Option<String>>,
}

impl Gig {
    /// Builds a new gig from the caller's `CreateGigPayload` plus the
    /// `user_id` it doesn't carry itself. Takes the payload by value
    /// (rather than one argument per field) to keep the constructor's
    /// arity fixed as fields are added.
    pub fn new(payload: CreateGigPayload, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            user_id,
            band_id: payload.band_id,
            setlist_id: payload.setlist_id,
            venue: payload.venue,
            location: payload.location,
            scheduled_at: payload.scheduled_at,
            status: payload.status.unwrap_or_default(),
            notes: payload.notes,
            share_token: None,
            share_locked_at: None,
            share_lock_reason: None,
            updated_by: None,
            updated_by_username: None,
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
    pub location: Option<String>,
    pub scheduled_at: NaiveDateTime,
    pub status: GigStatus,
    pub setlist: Option<crate::models::setlist::PublicSetlist>,
}
