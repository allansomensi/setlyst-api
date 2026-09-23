//! Song suggestions: a band member proposes a song for one of the band's
//! setlists (the repertoire by default), members vote, and someone allowed
//! to manage setlists accepts or rejects it — or it is accepted
//! automatically once it reaches the band's vote threshold.

use crate::models::{link::Links, song::Tonality};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::prelude::{FromRow, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "suggestion_status", rename_all = "lowercase")]
pub enum SuggestionStatus {
    Open,
    Accepted,
    Rejected,
    Withdrawn,
}

impl SuggestionStatus {
    pub fn key(&self) -> &'static str {
        match self {
            SuggestionStatus::Open => "open",
            SuggestionStatus::Accepted => "accepted",
            SuggestionStatus::Rejected => "rejected",
            SuggestionStatus::Withdrawn => "withdrawn",
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestionSetlist {
    pub id: Uuid,
    pub title: String,
    pub is_repertoire: bool,
}

/// The suggested song as it is now (`null` on the suggestion once it was
/// deleted; `song_title`/`artist_name` keep the snapshot).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestionSong {
    pub id: Uuid,
    pub title: String,
    pub artist_name: String,
    pub tonality: Option<Tonality>,
    pub tempo: Option<i32>,
    pub energy: Option<i16>,
    pub duration: Option<i32>,
    pub links: Links,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestionUser {
    pub id: Uuid,
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct SuggestionVotes {
    pub up: i64,
    pub down: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Suggestion {
    pub id: Uuid,
    pub band_id: Uuid,
    pub setlist: SuggestionSetlist,
    pub song: Option<SuggestionSong>,
    pub song_title: String,
    pub artist_name: String,
    pub suggested_by: Option<SuggestionUser>,
    pub note: Option<String>,
    pub status: SuggestionStatus,
    pub votes: SuggestionVotes,
    /// The caller's vote: -1, 0 (none) or 1.
    pub my_vote: i16,
    pub resolved_by_username: Option<String>,
    pub resolved_at: Option<NaiveDateTime>,
    pub resolution_note: Option<String>,
    pub created_at: NaiveDateTime,
}

/// Flat row the repository reads; turned into a [`Suggestion`].
#[derive(Debug, Clone, FromRow)]
pub struct SuggestionRow {
    pub id: Uuid,
    pub band_id: Uuid,
    pub setlist_id: Uuid,
    pub setlist_title: String,
    pub setlist_is_repertoire: bool,
    pub song_id: Option<Uuid>,
    pub live_song_id: Option<Uuid>,
    pub live_song_title: Option<String>,
    pub live_artist_name: Option<String>,
    pub live_tonality: Option<Tonality>,
    pub live_tempo: Option<i32>,
    pub live_energy: Option<i16>,
    pub live_duration: Option<i32>,
    #[sqlx(json(nullable))]
    pub live_links: Option<Links>,
    pub live_song_band_id: Option<Uuid>,
    pub song_title: String,
    pub artist_name: String,
    pub suggested_by: Option<Uuid>,
    pub suggested_by_username: Option<String>,
    pub suggested_by_avatar_url: Option<String>,
    pub note: Option<String>,
    pub status: SuggestionStatus,
    pub votes_up: i64,
    pub votes_down: i64,
    pub my_vote: i16,
    pub resolved_by_username: Option<String>,
    pub resolved_at: Option<NaiveDateTime>,
    pub resolution_note: Option<String>,
    pub created_at: NaiveDateTime,
}

impl From<SuggestionRow> for Suggestion {
    fn from(row: SuggestionRow) -> Self {
        let song = match (row.live_song_id, row.live_song_title) {
            (Some(id), Some(title)) => Some(SuggestionSong {
                id,
                title,
                artist_name: row.live_artist_name.unwrap_or_default(),
                tonality: row.live_tonality,
                tempo: row.live_tempo,
                energy: row.live_energy,
                duration: row.live_duration,
                links: row.live_links.unwrap_or_default(),
            }),
            _ => None,
        };
        let suggested_by = match (row.suggested_by, row.suggested_by_username) {
            (Some(id), Some(username)) => Some(SuggestionUser {
                id,
                username,
                avatar_url: row.suggested_by_avatar_url,
            }),
            _ => None,
        };
        Self {
            id: row.id,
            band_id: row.band_id,
            setlist: SuggestionSetlist {
                id: row.setlist_id,
                title: row.setlist_title,
                is_repertoire: row.setlist_is_repertoire,
            },
            song,
            song_title: row.song_title,
            artist_name: row.artist_name,
            suggested_by,
            note: row.note,
            status: row.status,
            votes: SuggestionVotes {
                up: row.votes_up,
                down: row.votes_down,
            },
            my_vote: row.my_vote,
            resolved_by_username: row.resolved_by_username,
            resolved_at: row.resolved_at,
            resolution_note: row.resolution_note,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct CreateSuggestionPayload {
    /// One of the caller's personal songs, or a song the band already owns.
    pub song_id: Uuid,
    /// A setlist of the band (default: its repertoire).
    pub setlist_id: Option<Uuid>,
    #[validate(length(max = 500, message = "The note must be at most 500 characters."))]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema, Validate)]
pub struct VotePayload {
    /// `1` (for) or `-1` (against).
    #[validate(custom(function = "validate_vote"))]
    pub value: i16,
}

fn validate_vote(value: i16) -> Result<(), validator::ValidationError> {
    if value == 1 || value == -1 {
        Ok(())
    } else {
        let mut error = validator::ValidationError::new("invalid_vote");
        error.message = Some(std::borrow::Cow::from("A vote is 1 or -1."));
        Err(error)
    }
}

#[derive(Debug, Default, Deserialize, Serialize, ToSchema, Validate)]
pub struct ResolveSuggestionPayload {
    #[validate(length(max = 500, message = "The note must be at most 500 characters."))]
    pub note: Option<String>,
}

/// Which suggestions to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionFilter {
    #[default]
    Open,
    Accepted,
    Rejected,
    Withdrawn,
    All,
}

impl SuggestionFilter {
    pub fn status(&self) -> Option<SuggestionStatus> {
        match self {
            SuggestionFilter::Open => Some(SuggestionStatus::Open),
            SuggestionFilter::Accepted => Some(SuggestionStatus::Accepted),
            SuggestionFilter::Rejected => Some(SuggestionStatus::Rejected),
            SuggestionFilter::Withdrawn => Some(SuggestionStatus::Withdrawn),
            SuggestionFilter::All => None,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestionListQuery {
    pub status: Option<SuggestionFilter>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// Whether a suggestion with these votes is accepted automatically under
/// the band's `threshold` (`None` = never): enough up-votes, and more up
/// than down.
pub fn reaches_auto_accept(threshold: Option<i32>, up: i64, down: i64) -> bool {
    match threshold {
        Some(threshold) if threshold > 0 => up >= i64::from(threshold) && up > down,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_accept_threshold() {
        assert!(!reaches_auto_accept(None, 10, 0));
        assert!(!reaches_auto_accept(Some(3), 2, 0));
        assert!(reaches_auto_accept(Some(3), 3, 0));
        assert!(reaches_auto_accept(Some(3), 3, 2));
        assert!(!reaches_auto_accept(Some(3), 3, 3));
        assert!(!reaches_auto_accept(Some(3), 3, 4));
        assert!(reaches_auto_accept(Some(1), 1, 0));
        assert!(!reaches_auto_accept(Some(0), 5, 0));
    }

    #[test]
    fn votes_are_one_or_minus_one() {
        assert!(VotePayload { value: 1 }.validate().is_ok());
        assert!(VotePayload { value: -1 }.validate().is_ok());
        assert!(VotePayload { value: 0 }.validate().is_err());
        assert!(VotePayload { value: 2 }.validate().is_err());
    }
}
