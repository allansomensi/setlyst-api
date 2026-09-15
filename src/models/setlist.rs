use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::prelude::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use validator::Validate;

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
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: String,
    pub description: Option<String>,
    /// Optionally create the setlist under a band instead of personally.
    /// The caller must be a member of the band with permission to manage its setlists.
    pub band_id: Option<Uuid>,
}

#[derive(Deserialize, Serialize, ToSchema, Validate)]
pub struct UpdateSetlistPayload {
    #[validate(length(min = 1, max = 255, message = "Title must be between 1 and 255 chars."))]
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
pub struct AddSongToSetlistPayload {
    pub song_id: Uuid,
    pub position: i32,
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
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct ReorderSetlistSongsPayload {
    #[validate(length(min = 1, message = "The list of song IDs cannot be empty."))]
    pub song_ids: Vec<Uuid>,
}

/// The read-only shape returned by the public (unauthenticated) setlist
/// endpoints. Deliberately excludes internal fields — `id`, `user_id`,
/// `band_id`, `share_token` — that an anonymous viewer has no use for and
/// that shouldn't be handed out to the internet.
#[derive(Debug, Serialize, ToSchema)]
pub struct PublicSetlist {
    pub title: String,
    pub description: Option<String>,
    pub total_duration: i32,
    pub songs: Vec<crate::models::song::SongWithArtist>,
}
