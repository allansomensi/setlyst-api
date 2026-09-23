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
    /// Added after the first release — absent in older backups.
    #[serde(default)]
    pub tags: Vec<String>,
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

/// Largest number of records of each kind a single backup may carry.
pub const MAX_BACKUP_RECORDS: usize = 20_000;

impl BackupFile {
    /// Structural validation run before anything touches the database, so
    /// a malformed or hand-edited file fails with a clear 400 instead of a
    /// database error halfway through the import.
    pub fn validate_contents(&self) -> Result<(), String> {
        use crate::validations::text::{MAX_DESCRIPTION_LENGTH, MAX_LYRICS_LENGTH};

        if self.version == 0 || self.version > BACKUP_FORMAT_VERSION {
            return Err(format!(
                "Unsupported backup version {} (this server reads version {BACKUP_FORMAT_VERSION}).",
                self.version
            ));
        }

        for (label, len) in [
            ("artists", self.artists.len()),
            ("songs", self.songs.len()),
            ("setlists", self.setlists.len()),
            ("gigs", self.gigs.len()),
        ] {
            if len > MAX_BACKUP_RECORDS {
                return Err(format!("The backup has too many {label} ({len})."));
            }
        }

        let bounded = |value: &str, max: usize| {
            let len = value.trim().chars().count();
            len >= 1 && len <= max
        };

        for artist in &self.artists {
            if !bounded(&artist.name, 255) {
                return Err("An artist has an empty or too long name.".to_string());
            }
        }
        for song in &self.songs {
            if !bounded(&song.title, 255) {
                return Err("A song has an empty or too long title.".to_string());
            }
            if song
                .lyrics
                .as_deref()
                .is_some_and(|l| l.chars().count() > MAX_LYRICS_LENGTH)
            {
                return Err(format!("The lyrics of \"{}\" are too long.", song.title));
            }
            if song.tempo.is_some_and(|t| !(1..=500).contains(&t)) {
                return Err(format!("\"{}\" has an invalid BPM.", song.title));
            }
            if song.duration.is_some_and(|d| !(1..=7_200).contains(&d)) {
                return Err(format!("\"{}\" has an invalid duration.", song.title));
            }
        }
        for setlist in &self.setlists {
            if !bounded(&setlist.title, 255) {
                return Err("A setlist has an empty or too long title.".to_string());
            }
            if setlist
                .description
                .as_deref()
                .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION_LENGTH)
            {
                return Err(format!(
                    "The description of \"{}\" is too long.",
                    setlist.title
                ));
            }
        }
        for gig in &self.gigs {
            if !bounded(&gig.venue, 255) {
                return Err("A gig has an empty or too long venue.".to_string());
            }
            if gig
                .notes
                .as_deref()
                .is_some_and(|n| n.chars().count() > MAX_DESCRIPTION_LENGTH)
            {
                return Err(format!(
                    "The notes of the gig at \"{}\" are too long.",
                    gig.venue
                ));
            }
        }
        Ok(())
    }
}
