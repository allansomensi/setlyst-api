use crate::models::gig::GigStatus;
use crate::models::link::LinkInput;
use crate::models::setlist::SetlistMarkerType;
use crate::models::song::{
    Genre, Tonality, validate_performance_notes, validate_time_signature, validate_tuning,
};
use crate::validations::{link::MAX_LINKS, tag::MAX_TAGS_PER_SONG};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use utoipa::ToSchema;
use uuid::Uuid;

/// Current version of the backup format.
/// Increment this constant if breaking schema changes are made.
///
/// - 1: artists, songs (with tags), setlists, personal gigs.
/// - 2: song energy, time signature, capo, tuning, performance notes and
///   links; setlist links; tours and each gig's tour and location. Every
///   new field is optional, so version 1 files still import.
/// - 3: song versions (`version_label`, `version_of`) and the key each
///   setlist plays a song in (`transpose`). Optional as well.
/// - 4: each song's harmonic analysis (`analysis`). Optional as well.
/// - 5: each setlist's blocks and breaks (`markers`), and files holding a
///   single setlist, gig or tour with what it needs (`kind`; see
///   `GET /setlists/{id}/export`, `/gigs/{id}/export`, `/tours/{id}/export`).
///   Optional as well.
pub const BACKUP_FORMAT_VERSION: u32 = 5;

/// What a file holds: a whole account, or one setlist, gig or tour with
/// the setlists, songs and artists it needs (since version 5; older files
/// are backups).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    #[default]
    Backup,
    Setlist,
    Gig,
    Tour,
}

/// A fully self-contained, portable snapshot of a user's data.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupFile {
    pub version: u32,
    /// Since version 5.
    #[serde(default)]
    pub kind: BackupKind,
    pub exported_at: NaiveDateTime,
    pub artists: Vec<BackupArtist>,
    pub songs: Vec<BackupSong>,
    pub setlists: Vec<BackupSetlist>,
    #[serde(default)]
    pub gigs: Vec<BackupGig>,
    /// Since version 2.
    #[serde(default)]
    pub tours: Vec<BackupTour>,
}

/// Tour entry inside a backup file (personal tours only).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupTour {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
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
    /// Since version 2.
    #[serde(default)]
    pub energy: Option<i16>,
    #[serde(default)]
    pub time_signature: Option<String>,
    #[serde(default)]
    pub capo: Option<i16>,
    #[serde(default)]
    pub tuning: Option<String>,
    #[serde(default)]
    pub performance_notes: Option<String>,
    #[serde(default)]
    pub links: Vec<LinkInput>,
    /// Since version 3: what sets this version apart ("Simplified").
    #[serde(default)]
    pub version_label: Option<String>,
    /// The `id` (in this file) of the original this song is a version of.
    #[serde(default)]
    pub version_of: Option<Uuid>,
    /// Since version 4: the song's harmonic analysis document (see
    /// `PUT /songs/{id}/analysis`). On import it only fills songs that
    /// don't have one yet.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub analysis: Option<serde_json::Value>,
}

/// Setlist entry inside a backup file.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupSetlist {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub songs: Vec<BackupSetlistSong>,
    /// Since version 2.
    #[serde(default)]
    pub links: Vec<LinkInput>,
    /// Since version 5: block headers and breaks, in the same position
    /// space as `songs`.
    #[serde(default)]
    pub markers: Vec<BackupMarker>,
}

/// A block header or a break inside a backed-up setlist.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupMarker {
    pub marker_type: SetlistMarkerType,
    /// The block's name (required for blocks) or the break's label.
    #[serde(default)]
    pub label: Option<String>,
    /// Breaks only.
    #[serde(default)]
    pub duration_minutes: Option<i32>,
    pub position: i32,
}

/// A song reference within a setlist, preserving its display position.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BackupSetlistSong {
    pub song_id: Uuid,
    pub position: i32,
    /// Since version 3: the key the setlist plays it in, in semitones from
    /// the song's written key.
    #[serde(default)]
    pub transpose: i16,
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
    /// Since version 2.
    #[serde(default)]
    pub location: Option<String>,
    /// References a [`BackupTour::id`] in the same file (since version 2).
    #[serde(default)]
    pub tour_id: Option<Uuid>,
}

/// Summary returned to the caller after a successful import.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ImportSummary {
    pub artists_imported: usize,
    pub songs_imported: usize,
    pub setlists_imported: usize,
    #[serde(default)]
    pub gigs_imported: usize,
    #[serde(default)]
    pub tours_imported: usize,
    /// Tours in the file left out because the plan doesn't include the
    /// `tours` feature (their gigs are imported without a tour).
    #[serde(default)]
    pub skipped_tours: usize,
    /// The setlists, gigs and tours created, in the file's order (since
    /// version 5): where to go after importing a shared setlist, gig or
    /// tour.
    #[serde(default)]
    pub setlist_ids: Vec<Uuid>,
    #[serde(default)]
    pub gig_ids: Vec<Uuid>,
    #[serde(default)]
    pub tour_ids: Vec<Uuid>,
}

/// Largest number of records of each kind a single backup may carry.
pub const MAX_BACKUP_RECORDS: usize = 20_000;

/// Largest song position accepted inside a backed-up setlist.
pub const MAX_BACKUP_POSITION: i32 = 1_000_000;

impl BackupKind {
    fn noun(self) -> &'static str {
        match self {
            BackupKind::Backup => "full backup",
            BackupKind::Setlist => "setlist",
            BackupKind::Gig => "gig",
            BackupKind::Tour => "tour",
        }
    }
}

impl BackupFile {
    /// Checks that the file is a shared file of `kind` and holds what such
    /// a file holds, and nothing else — a backup (older files carry no
    /// `kind`) would bring a whole library along:
    ///
    /// - a setlist file: one setlist;
    /// - a gig file: one gig and, if it has one, its setlist;
    /// - a tour file: one tour, its gigs and their setlists.
    ///
    /// Every gig points at a setlist of the file (or none), and every
    /// setlist is some gig's.
    pub fn check_shared(&self, kind: BackupKind) -> Result<(), String> {
        if self.kind != kind {
            return Err(format!(
                "This file holds a {}, not a {}.",
                self.kind.noun(),
                kind.noun()
            ));
        }
        let setlist_ids: HashSet<Uuid> = self.setlists.iter().map(|s| s.id).collect();
        if setlist_ids.len() != self.setlists.len() {
            return Err("The file lists a setlist twice.".to_string());
        }
        let shape = match kind {
            BackupKind::Backup => false,
            BackupKind::Setlist => {
                self.setlists.len() == 1 && self.gigs.is_empty() && self.tours.is_empty()
            }
            BackupKind::Gig => {
                self.gigs.len() == 1 && self.setlists.len() <= 1 && self.tours.is_empty()
            }
            BackupKind::Tour => {
                self.tours.len() == 1
                    && self
                        .gigs
                        .iter()
                        .all(|gig| gig.tour_id == Some(self.tours[0].id))
            }
        };
        if !shape {
            return Err(format!(
                "A {} file holds one {} and what it needs.",
                kind.noun(),
                kind.noun()
            ));
        }
        if kind != BackupKind::Setlist {
            let used: HashSet<Uuid> = self.gigs.iter().filter_map(|g| g.setlist_id).collect();
            if kind == BackupKind::Gig && self.gigs[0].tour_id.is_some() {
                return Err("A gig file holds no tour.".to_string());
            }
            if used != setlist_ids {
                return Err("Every setlist in the file must be one of its gigs'.".to_string());
            }
        }
        Ok(())
    }

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
            ("tours", self.tours.len()),
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
            if song.energy.is_some_and(|e| !(1..=5).contains(&e)) {
                return Err(format!("\"{}\" has an invalid energy.", song.title));
            }
            if song.capo.is_some_and(|c| !(0..=11).contains(&c)) {
                return Err(format!("\"{}\" has an invalid capo.", song.title));
            }
            if song
                .version_label
                .as_deref()
                .is_some_and(|l| crate::models::song::validate_version_label(l).is_err())
            {
                return Err(format!("\"{}\" has an invalid version name.", song.title));
            }
            if song
                .time_signature
                .as_deref()
                .is_some_and(|t| validate_time_signature(t).is_err())
            {
                return Err(format!("\"{}\" has an invalid time signature.", song.title));
            }
            if song
                .tuning
                .as_deref()
                .is_some_and(|t| validate_tuning(t).is_err())
            {
                return Err(format!("\"{}\" has an invalid tuning.", song.title));
            }
            if song
                .performance_notes
                .as_deref()
                .is_some_and(|n| validate_performance_notes(n).is_err())
            {
                return Err(format!(
                    "The performance notes of \"{}\" are too long.",
                    song.title
                ));
            }
            if song.links.len() > MAX_LINKS {
                return Err(format!("\"{}\" has too many links.", song.title));
            }
            if song.tags.len() > MAX_TAGS_PER_SONG {
                return Err(format!("\"{}\" has too many tags.", song.title));
            }
            if let Some(analysis) = &song.analysis
                && crate::models::song_analysis::check_content(analysis).is_err()
            {
                return Err(format!(
                    "The harmonic analysis of \"{}\" is invalid or too large.",
                    song.title
                ));
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
            if setlist.links.len() > MAX_LINKS {
                return Err(format!("\"{}\" has too many links.", setlist.title));
            }
            if setlist.songs.len() > MAX_BACKUP_RECORDS {
                return Err(format!("\"{}\" has too many songs.", setlist.title));
            }
            if setlist.markers.len() > MAX_BACKUP_RECORDS {
                return Err(format!(
                    "\"{}\" has too many blocks and breaks.",
                    setlist.title
                ));
            }
            for marker in &setlist.markers {
                if !(0..=MAX_BACKUP_POSITION).contains(&marker.position) {
                    return Err(format!(
                        "\"{}\" has a block or break position outside 0..={MAX_BACKUP_POSITION}.",
                        setlist.title
                    ));
                }
                let label = marker.label.as_deref().map(str::trim).unwrap_or_default();
                let valid = match marker.marker_type {
                    SetlistMarkerType::Block => {
                        bounded(label, 255) && marker.duration_minutes.is_none()
                    }
                    SetlistMarkerType::Break => {
                        label.chars().count() <= 255
                            && marker
                                .duration_minutes
                                .is_none_or(|d| (0..=1440).contains(&d))
                    }
                };
                if !valid {
                    return Err(format!(
                        "\"{}\" has an invalid block or break.",
                        setlist.title
                    ));
                }
            }
            // Positions are appended to (`MAX(position) + 1`) later on; a
            // value near `i32::MAX` would make every later add overflow.
            if setlist.songs.iter().any(|entry| {
                !(0..=MAX_BACKUP_POSITION).contains(&entry.position)
                    || !(-11..=11).contains(&entry.transpose)
            }) {
                return Err(format!(
                    "\"{}\" has a song position outside 0..={MAX_BACKUP_POSITION} or a key change outside -11..=11.",
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
            if gig
                .location
                .as_deref()
                .is_some_and(|l| l.chars().count() > 500)
            {
                return Err(format!(
                    "The location of the gig at \"{}\" is too long.",
                    gig.venue
                ));
            }
        }
        for tour in &self.tours {
            if !bounded(&tour.name, 120) || tour.name.chars().any(char::is_control) {
                return Err("A tour has an empty or too long name.".to_string());
            }
            if tour
                .description
                .as_deref()
                .is_some_and(|d| d.chars().count() > MAX_DESCRIPTION_LENGTH)
            {
                return Err(format!(
                    "The description of the tour \"{}\" is too long.",
                    tour.name
                ));
            }
            if tour.end_date < tour.start_date {
                return Err(format!("The tour \"{}\" ends before it starts.", tour.name));
            }
        }
        Ok(())
    }
}
