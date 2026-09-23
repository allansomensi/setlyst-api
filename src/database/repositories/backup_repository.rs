use crate::{
    errors::api_error::ApiError,
    models::{
        backup::{
            BACKUP_FORMAT_VERSION, BackupArtist, BackupFile, BackupGig, BackupSetlist,
            BackupSetlistSong, BackupSong, BackupTour, ImportSummary,
        },
        link::{LinkInput, StoredLink},
        quota::QuotaLimits,
        song::clean_text,
    },
    validations::{link::normalize_links, tag::normalize_tags},
};
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::error;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct ArtistRow {
    id: Uuid,
    name: String,
}

#[derive(sqlx::FromRow)]
struct SongRow {
    id: Uuid,
    title: String,
    artist_id: Uuid,
    tempo: Option<i32>,
    lyrics: Option<String>,
    tonality: Option<crate::models::song::Tonality>,
    genre: Option<crate::models::song::Genre>,
    duration: Option<i32>,
    tags: Vec<String>,
    energy: Option<i16>,
    time_signature: Option<String>,
    capo: Option<i16>,
    tuning: Option<String>,
    performance_notes: Option<String>,
    links: sqlx::types::Json<Vec<StoredLink>>,
}

#[derive(sqlx::FromRow)]
struct SetlistRow {
    id: Uuid,
    title: String,
    description: Option<String>,
    links: sqlx::types::Json<Vec<StoredLink>>,
}

#[derive(sqlx::FromRow)]
struct SetlistSongRow {
    setlist_id: Uuid,
    song_id: Uuid,
    position: i32,
}

#[derive(sqlx::FromRow)]
struct GigRow {
    id: Uuid,
    venue: String,
    location: Option<String>,
    scheduled_at: chrono::NaiveDateTime,
    setlist_id: Option<Uuid>,
    status: crate::models::gig::GigStatus,
    notes: Option<String>,
    tour_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct TourRow {
    id: Uuid,
    name: String,
    description: Option<String>,
    start_date: NaiveDate,
    end_date: NaiveDate,
}

fn to_inputs(links: Vec<StoredLink>) -> Vec<LinkInput> {
    links
        .into_iter()
        .map(|l| LinkInput {
            url: l.url,
            label: l.label,
        })
        .collect()
}

#[async_trait::async_trait]
pub trait BackupRepository: Send + Sync {
    /// Collects all of a user's live personal artists, songs, setlists,
    /// gigs and tours and returns them as a portable, self-contained
    /// [`BackupFile`].
    async fn export(&self, user_id: Uuid) -> Result<BackupFile, ApiError>;

    /// Atomically imports a [`BackupFile`] (current or older version) into
    /// the target user's account.
    ///
    /// **Merge rules:**
    /// - Artists with the same name that already exist are reused, not duplicated.
    /// - Songs with the same (title, artist, user) that already exist are reused.
    /// - Setlists, gigs and tours are always created as new entries.
    /// - All IDs are remapped to fresh UUIDs; the backup IDs are only used as
    ///   reference keys during the import phase.
    ///
    /// Everything is validated before anything is written (lengths, ranges,
    /// links, per-setlist item limits), and the whole import runs in one
    /// transaction; any failure rolls back completely. When `limits` is
    /// given, the account's totals are checked against them *after*
    /// merging (so re-importing overlapping data isn't penalized) and the
    /// whole import is rolled back if any is exceeded.
    async fn import(
        &self,
        user_id: Uuid,
        backup: BackupFile,
        limits: Option<QuotaLimits>,
    ) -> Result<ImportSummary, ApiError>;
}

pub struct BackupRepositoryImpl {
    pub db: PgPool,
}

impl BackupRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BackupRepository for BackupRepositoryImpl {
    async fn export(&self, user_id: Uuid) -> Result<BackupFile, ApiError> {
        let artists_fut = sqlx::query_as::<_, ArtistRow>(
            "SELECT id, name FROM artists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL ORDER BY name ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let songs_fut = sqlx::query_as::<_, SongRow>(
            "SELECT s.id, s.title, s.artist_id, s.tempo, s.lyrics, s.tonality, s.genre, s.duration,
                    COALESCE((SELECT array_agg(st.tag ORDER BY st.tag) FROM song_tags st WHERE st.song_id = s.id), '{}') AS tags,
                    s.energy, s.time_signature, s.capo, s.tuning, s.performance_notes, s.links
             FROM songs s
             WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL
             ORDER BY s.title ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let setlists_fut = sqlx::query_as::<_, SetlistRow>(
            "SELECT id, title, description, links FROM setlists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL ORDER BY title ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let gigs_fut = sqlx::query_as::<_, GigRow>(
            "SELECT g.id, g.venue, g.location, g.scheduled_at,
                    (SELECT s.id FROM setlists s WHERE s.id = g.setlist_id AND s.deleted_at IS NULL) AS setlist_id,
                    g.status, g.notes,
                    (SELECT t.id FROM tours t WHERE t.id = g.tour_id AND t.deleted_at IS NULL) AS tour_id
             FROM gigs g
             WHERE g.user_id = $1 AND g.band_id IS NULL AND g.deleted_at IS NULL
             ORDER BY g.scheduled_at ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let tours_fut = sqlx::query_as::<_, TourRow>(
            "SELECT id, name, description, start_date, end_date FROM tours
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL
             ORDER BY start_date ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let (artist_rows, song_rows, setlist_rows, gig_rows, tour_rows) =
            tokio::try_join!(artists_fut, songs_fut, setlists_fut, gigs_fut, tours_fut)?;

        let setlist_ids: Vec<Uuid> = setlist_rows.iter().map(|s| s.id).collect();

        // Only live personal songs of the owner (never a band's copy).
        let setlist_song_rows: Vec<SetlistSongRow> = if setlist_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, SetlistSongRow>(
                "SELECT ss.setlist_id, ss.song_id, ss.position
                 FROM setlist_songs ss
                 INNER JOIN songs s ON s.id = ss.song_id
                 WHERE ss.setlist_id = ANY($1) AND s.deleted_at IS NULL
                   AND s.band_id IS NULL AND s.user_id = $2
                 ORDER BY ss.setlist_id, ss.position ASC",
            )
            .bind(&setlist_ids)
            .bind(user_id)
            .fetch_all(&self.db)
            .await?
        };

        let mut songs_by_setlist: HashMap<Uuid, Vec<BackupSetlistSong>> = HashMap::new();
        for row in setlist_song_rows {
            songs_by_setlist
                .entry(row.setlist_id)
                .or_default()
                .push(BackupSetlistSong {
                    song_id: row.song_id,
                    position: row.position,
                });
        }

        let artists = artist_rows
            .into_iter()
            .map(|r| BackupArtist {
                id: r.id,
                name: r.name,
            })
            .collect();

        let songs = song_rows
            .into_iter()
            .map(|r| BackupSong {
                id: r.id,
                title: r.title,
                artist_id: r.artist_id,
                tempo: r.tempo,
                lyrics: r.lyrics,
                tonality: r.tonality,
                genre: r.genre,
                duration: r.duration,
                tags: r.tags,
                energy: r.energy,
                time_signature: r.time_signature,
                capo: r.capo,
                tuning: r.tuning,
                performance_notes: r.performance_notes,
                links: to_inputs(r.links.0),
            })
            .collect();

        let setlists = setlist_rows
            .into_iter()
            .map(|r| BackupSetlist {
                id: r.id,
                title: r.title,
                description: r.description,
                songs: songs_by_setlist.remove(&r.id).unwrap_or_default(),
                links: to_inputs(r.links.0),
            })
            .collect();

        let gigs = gig_rows
            .into_iter()
            .map(|r| BackupGig {
                id: r.id,
                venue: r.venue,
                scheduled_at: r.scheduled_at,
                setlist_id: r.setlist_id,
                status: r.status,
                notes: r.notes,
                location: r.location,
                tour_id: r.tour_id,
            })
            .collect();

        let tours = tour_rows
            .into_iter()
            .map(|r| BackupTour {
                id: r.id,
                name: r.name,
                description: r.description,
                start_date: r.start_date,
                end_date: r.end_date,
            })
            .collect();

        Ok(BackupFile {
            version: BACKUP_FORMAT_VERSION,
            exported_at: Utc::now().naive_utc(),
            artists,
            songs,
            setlists,
            gigs,
            tours,
        })
    }

    async fn import(
        &self,
        user_id: Uuid,
        backup: BackupFile,
        limits: Option<QuotaLimits>,
    ) -> Result<ImportSummary, ApiError> {
        backup.validate_contents().map_err(ApiError::BadRequest)?;

        // Everything that can fail validation is checked before writing.
        let song_links: Vec<Vec<StoredLink>> = backup
            .songs
            .iter()
            .map(|s| normalize_links(&s.links))
            .collect::<Result<_, _>>()?;
        let setlist_links: Vec<Vec<StoredLink>> = backup
            .setlists
            .iter()
            .map(|s| normalize_links(&s.links))
            .collect::<Result<_, _>>()?;
        if let Some(limits) = limits {
            for setlist in &backup.setlists {
                let distinct: HashSet<Uuid> = setlist.songs.iter().map(|s| s.song_id).collect();
                if distinct.len() as i64 > limits.setlist_items {
                    return Err(ApiError::quota_exceeded(
                        "setlist_items",
                        limits.setlist_items,
                    ));
                }
            }
        }

        let now = Utc::now().naive_utc();

        let artists_incoming = backup.artists.len();
        let songs_incoming = backup.songs.len();
        let setlists_incoming = backup.setlists.len();
        let gigs_incoming = backup.gigs.len();
        let tours_incoming = backup.tours.len();

        let mut tx = self.db.begin().await?;

        let mut artist_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(artists_incoming);

        for artist in &backup.artists {
            let name = artist.name.trim();
            let resolved_id: Uuid = match sqlx::query_scalar(
                "SELECT id FROM artists
                 WHERE LOWER(name) = LOWER($1) AND user_id = $2 AND band_id IS NULL AND deleted_at IS NULL",
            )
            .bind(name)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                Some(existing_id) => existing_id,
                None => {
                    let new_id = Uuid::new_v4();
                    sqlx::query(
                        "INSERT INTO artists (id, name, user_id, created_at, updated_at)
                         VALUES ($1, $2, $3, $4, $4)",
                    )
                    .bind(new_id)
                    .bind(name)
                    .bind(user_id)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        error!("Failed to insert an artist during import: {e}");
                        ApiError::DatabaseError(e)
                    })?;
                    new_id
                }
            };

            artist_id_map.insert(artist.id, resolved_id);
        }

        let mut song_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(songs_incoming);
        for (song, links) in backup.songs.iter().zip(song_links) {
            let Some(&resolved_artist_id) = artist_id_map.get(&song.artist_id) else {
                return Err(ApiError::BadRequest(format!(
                    "Invalid backup: the song \"{}\" references an artist that isn't in the file.",
                    song.title
                )));
            };
            let title = song.title.trim();

            let resolved_id: Uuid = match sqlx::query_scalar(
                "SELECT id FROM songs
                 WHERE LOWER(TRIM(title)) = LOWER(TRIM($1)) AND artist_id = $2 AND user_id = $3
                   AND band_id IS NULL AND deleted_at IS NULL",
            )
            .bind(title)
            .bind(resolved_artist_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                Some(existing_id) => existing_id,
                None => {
                    let new_id = Uuid::new_v4();
                    let lyrics = song.lyrics.as_deref().filter(|l| !l.trim().is_empty());
                    sqlx::query(
                        "INSERT INTO songs
                         (id, title, artist_id, user_id, tempo, lyrics, tonality, genre, duration,
                          energy, time_signature, capo, tuning, performance_notes, links,
                          created_at, updated_at)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16)",
                    )
                    .bind(new_id)
                    .bind(title)
                    .bind(resolved_artist_id)
                    .bind(user_id)
                    .bind(song.tempo)
                    .bind(lyrics)
                    .bind(song.tonality)
                    .bind(song.genre)
                    .bind(song.duration)
                    .bind(song.energy)
                    .bind(clean_text(song.time_signature.as_deref()))
                    .bind(song.capo)
                    .bind(clean_text(song.tuning.as_deref()))
                    .bind(clean_text(song.performance_notes.as_deref()))
                    .bind(sqlx::types::Json(&links))
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        error!("Failed to insert a song during import: {e}");
                        ApiError::DatabaseError(e)
                    })?;
                    new_id
                }
            };

            let tags = normalize_tags(&song.tags).unwrap_or_default();
            if !tags.is_empty() {
                sqlx::query(
                    "INSERT INTO song_tags (song_id, tag, created_at)
                     SELECT $1, t, $3 FROM UNNEST($2::text[]) AS t
                     ON CONFLICT DO NOTHING",
                )
                .bind(resolved_id)
                .bind(&tags)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }

            song_id_map.insert(song.id, resolved_id);
        }

        let mut setlist_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(setlists_incoming);

        for (setlist, links) in backup.setlists.iter().zip(setlist_links) {
            let new_setlist_id = Uuid::new_v4();
            let description = setlist
                .description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty());

            sqlx::query(
                "INSERT INTO setlists (id, title, description, user_id, links, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $6)",
            )
            .bind(new_setlist_id)
            .bind(setlist.title.trim())
            .bind(description)
            .bind(user_id)
            .bind(sqlx::types::Json(&links))
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to insert a setlist during import: {e}");
                ApiError::DatabaseError(e)
            })?;

            setlist_id_map.insert(setlist.id, new_setlist_id);

            let mut song_ids: Vec<Uuid> = Vec::with_capacity(setlist.songs.len());
            let mut positions: Vec<i32> = Vec::with_capacity(setlist.songs.len());
            for entry in &setlist.songs {
                let Some(&resolved_song_id) = song_id_map.get(&entry.song_id) else {
                    return Err(ApiError::BadRequest(format!(
                        "Invalid backup: the setlist \"{}\" references a song that isn't in the file.",
                        setlist.title
                    )));
                };
                // A song listed twice keeps its last position.
                if let Some(index) = song_ids.iter().position(|id| *id == resolved_song_id) {
                    positions[index] = entry.position;
                } else {
                    song_ids.push(resolved_song_id);
                    positions.push(entry.position);
                }
            }

            if !song_ids.is_empty() {
                sqlx::query(
                    "INSERT INTO setlist_songs (setlist_id, song_id, position)
                     SELECT $1, t.song_id, t.position
                     FROM UNNEST($2::uuid[], $3::int[]) AS t(song_id, position)",
                )
                .bind(new_setlist_id)
                .bind(&song_ids)
                .bind(&positions)
                .execute(&mut *tx)
                .await?;
            }
        }

        let mut tour_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(tours_incoming);
        for tour in &backup.tours {
            let new_tour_id = Uuid::new_v4();
            let description = tour
                .description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty());
            sqlx::query(
                "INSERT INTO tours (id, user_id, band_id, name, description, start_date, end_date, created_at, updated_at)
                 VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $7)",
            )
            .bind(new_tour_id)
            .bind(user_id)
            .bind(tour.name.trim())
            .bind(description)
            .bind(tour.start_date)
            .bind(tour.end_date)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            tour_id_map.insert(tour.id, new_tour_id);
        }

        for gig in &backup.gigs {
            let new_gig_id = Uuid::new_v4();
            // Unlike songs referenced by a setlist, a dangling setlist or
            // tour reference on a gig is not fatal to the whole import —
            // the gig itself still carries useful information (venue,
            // date, notes) on its own, so we just leave it unlinked.
            let resolved_setlist_id = gig
                .setlist_id
                .and_then(|id| setlist_id_map.get(&id).copied());
            let resolved_tour_id = gig.tour_id.and_then(|id| tour_id_map.get(&id).copied());

            sqlx::query(
                "INSERT INTO gigs (id, user_id, band_id, setlist_id, venue, location, scheduled_at, status, notes, tour_id, created_at, updated_at)
                 VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
            )
            .bind(new_gig_id)
            .bind(user_id)
            .bind(resolved_setlist_id)
            .bind(gig.venue.trim())
            .bind(clean_text(gig.location.as_deref()))
            .bind(gig.scheduled_at)
            .bind(gig.status)
            .bind(clean_text(gig.notes.as_deref()))
            .bind(resolved_tour_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to insert a gig during import: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        if let Some(limits) = limits {
            let (artists, songs, setlists, gigs, tags, tours): (i64, i64, i64, i64, i64, i64) =
                sqlx::query_as(
                    "SELECT
                    (SELECT COUNT(*) FROM artists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                    (SELECT COUNT(*) FROM songs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                    (SELECT COUNT(*) FROM setlists WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                    (SELECT COUNT(*) FROM gigs WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL),
                    (SELECT COUNT(DISTINCT st.tag) FROM song_tags st
                        INNER JOIN songs s ON s.id = st.song_id
                        WHERE s.user_id = $1 AND s.band_id IS NULL AND s.deleted_at IS NULL),
                    (SELECT COUNT(*) FROM tours WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL)",
                )
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;

            for (resource, used, limit) in [
                ("artists", artists, limits.artists),
                ("songs", songs, limits.songs),
                ("setlists", setlists, limits.setlists),
                ("gigs", gigs, limits.gigs),
                ("tags", tags, limits.tags),
                ("tours", tours, limits.tours),
            ] {
                if used > limit {
                    tx.rollback().await?;
                    return Err(ApiError::quota_exceeded(resource, limit));
                }
            }
        }

        tx.commit().await?;

        Ok(ImportSummary {
            artists_imported: artists_incoming,
            songs_imported: songs_incoming,
            setlists_imported: setlists_incoming,
            gigs_imported: gigs_incoming,
            tours_imported: tours_incoming,
        })
    }
}
