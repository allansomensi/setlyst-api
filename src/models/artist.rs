use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

#[derive(ToSchema, Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Artist {
    pub id: Uuid,
    pub name: String,
    pub user_id: Uuid,
    /// The band that owns this artist as an independent, shared copy, or
    /// `None` for a personal artist. See [`crate::models::song::Song::band_id`].
    pub band_id: Option<Uuid>,
    /// The personal artist this band-owned copy was resolved from, if any
    /// — `None` for a personal artist, or for a band artist that was
    /// created fresh rather than forked from an existing one. Mirrors
    /// [`crate::models::song::Song::forked_from`]; lets platform-wide
    /// metrics avoid double-counting the same artist once per scope.
    pub forked_from: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by: Option<Uuid>,
    #[sqlx(default)]
    pub updated_by_username: Option<String>,
    /// Number of songs by this artist in the same scope (personal list).
    #[sqlx(default)]
    pub song_count: i64,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateArtistPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Artist name must be between 1 and 255 chars."
    ))]
    pub name: String,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateArtistPayload {
    #[validate(length(
        min = 1,
        max = 255,
        message = "Artist name must be between 1 and 255 chars."
    ))]
    pub name: Option<String>,
}

impl Artist {
    pub fn new(name: &str, user_id: Uuid) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            user_id,
            band_id: None,
            forked_from: None,
            updated_by: None,
            updated_by_username: None,
            song_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Creates a band-owned artist, e.g. resolved while forking a song into
    /// a band (see [`crate::models::song::Song::fork_for_band`]).
    /// `creator_id` is kept for audit purposes only; it does not grant that
    /// user any special ownership over the result. `forked_from` should be
    /// the personal artist this one mirrors, if the caller resolved one by
    /// name — `None` if this is genuinely new content created for the band.
    pub fn new_for_band(
        name: &str,
        band_id: Uuid,
        creator_id: Uuid,
        forked_from: Option<Uuid>,
    ) -> Self {
        let now = Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            user_id: creator_id,
            band_id: Some(band_id),
            forked_from,
            updated_by: None,
            updated_by_username: None,
            song_count: 0,
            created_at: now,
            updated_at: now,
        }
    }
}
