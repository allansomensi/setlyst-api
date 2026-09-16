use crate::models::gig::GigStatus;
use crate::models::song::{Genre, Tonality};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Current version of the backup format.
/// Increment this constant if breaking schema changes are made.
pub const BACKUP_FORMAT_VERSION: u32 = 1;

/// A fully self-contained, portable snapshot of a user's data.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupFile {
    pub version: u32,
    pub exported_at: NaiveDateTime,
    pub artists: Vec<BackupArtist>,
    pub songs: Vec<BackupSong>,
    pub setlists: Vec<BackupSetlist>,
    #[serde(default)]
    pub gigs: Vec<BackupGig>,
}

/// Artist entry inside a backup file.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupArtist {
    pub id: Uuid,
    pub name: String,
}

/// Song entry inside a backup file.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupSong {
    pub id: Uuid,
    pub title: String,
    pub artist_id: Uuid,
    pub tempo: Option<i32>,
    pub lyrics: Option<String>,
    pub tonality: Option<Tonality>,
    pub genre: Option<Genre>,
    pub duration: Option<i32>,
}

/// Setlist entry inside a backup file.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupSetlist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub songs: Vec<BackupSetlistSong>,
}

/// A song reference within a setlist, preserving its display position.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupSetlistSong {
    pub song_id: Uuid,
    pub position: i32,
}

/// Gig entry inside a backup file. Only personal (non-band) gigs are ever
/// included — band data isn't part of a personal backup, same as band
/// setlists.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupGig {
    pub id: Uuid,
    pub venue: String,
    pub scheduled_at: NaiveDateTime,
    /// References a [`BackupSetlist::id`] in the same file, if any. Left
    /// unset on import if the referenced setlist can't be resolved,
    /// rather than aborting the whole import over a missing link.
    pub setlist_id: Option<Uuid>,
    pub status: GigStatus,
    pub notes: Option<String>,
}

/// Summary returned to the caller after a successful import.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportSummary {
    pub artists_imported: usize,
    pub songs_imported: usize,
    pub setlists_imported: usize,
    #[serde(default)]
    pub gigs_imported: usize,
}
