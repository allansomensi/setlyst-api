use crate::{
    database::repositories::quota_repository::lock_scope,
    errors::api_error::{ApiError, codes},
    models::{
        backup::{
            BACKUP_FORMAT_VERSION, BackupArtist, BackupFile, BackupGig, BackupSetlist,
            BackupSetlistSong, BackupSong, BackupTour, ImportSummary,
        },
        link::{LinkInput, StoredLink},
        quota::{QuotaLimits, QuotaResource},
        song::clean_text,
    },
    validations::{link::normalize_links, tag::normalize_tags},
};
use axum::http::StatusCode;
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
    /// links, per-setlist item limits). When `limits` is given, the quota
    /// is checked twice: before the transaction, with every record of the
    /// file counted as new (an upper bound: a file that could never fit is
    /// refused without touching the database), and again inside it, after
    /// merging, on the account's real totals.
    ///
    /// The whole import runs in one transaction (any failure rolls back
    /// completely) with a 20-second statement timeout, and only one import
    /// per account runs at a time (`IMPORT_IN_PROGRESS`, 409). Without
    /// `allow_tours` (the plan lacks the `tours` feature) the file's tours
    /// are skipped and counted in `skipped_tours`.
    async fn import(
        &self,
        user_id: Uuid,
        backup: BackupFile,
        limits: Option<QuotaLimits>,
        allow_tours: bool,
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
        allow_tours: bool,
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

        let tours: &[BackupTour] = if allow_tours { &backup.tours } else { &[] };
        let skipped_tours = backup.tours.len() - tours.len();

        // Upper bound before any write: every record counted as new.
        if let Some(limits) = limits {
            let usage = usage_of(&self.db, user_id).await?;
            for (resource, used, incoming, limit) in [
                (
                    "artists",
                    usage.artists,
                    backup.artists.len(),
                    limits.artists,
                ),
                ("songs", usage.songs, backup.songs.len(), limits.songs),
                (
                    "setlists",
                    usage.setlists,
                    backup.setlists.len(),
                    limits.setlists,
                ),
                ("gigs", usage.gigs, backup.gigs.len(), limits.gigs),
                ("tours", usage.tours, tours.len(), limits.tours),
            ] {
                if used + incoming as i64 > limit {
                    return Err(ApiError::quota_exceeded(resource, limit));
                }
            }
        }

        let now = Utc::now().naive_utc();

        let mut tx = self.db.begin().await?;

        // One import per account at a time: a second one fails right away
        // instead of queueing behind the first (holding a connection).
        let locked: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended('backup_import:' || $1::text, 0))",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if !locked {
            return Err(import_in_progress());
        }
        sqlx::query("SET LOCAL statement_timeout = '20s'")
            .execute(&mut *tx)
            .await?;
        // Under the locks every creation in the account takes, so the
        // final quota check below and a concurrent `POST /songs` (or
        // restore) can't both pass on the same count.
        if limits.is_some() {
            for resource in [
                QuotaResource::Songs,
                QuotaResource::Artists,
                QuotaResource::Setlists,
                QuotaResource::Gigs,
                QuotaResource::Tags,
                QuotaResource::Tours,
            ] {
                lock_scope(&mut tx, resource, user_id).await?;
            }
        }

        // Artists: the account's existing ones by name, the rest inserted
        // in one statement.
        let existing_artists: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, LOWER(name) FROM artists
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL
             ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut artist_by_name: HashMap<String, Uuid> = HashMap::new();
        for (id, name) in existing_artists {
            artist_by_name.entry(name).or_insert(id);
        }
        let mut artist_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(backup.artists.len());
        let (mut new_artist_ids, mut new_artist_names) = (Vec::new(), Vec::new());
        for artist in &backup.artists {
            let name = artist.name.trim();
            let resolved = *artist_by_name
                .entry(name.to_lowercase())
                .or_insert_with(|| {
                    let id = Uuid::new_v4();
                    new_artist_ids.push(id);
                    new_artist_names.push(name.to_string());
                    id
                });
            artist_id_map.insert(artist.id, resolved);
        }
        if !new_artist_ids.is_empty() {
            sqlx::query(
                "INSERT INTO artists (id, name, user_id, created_at, updated_at)
                 SELECT t.id, t.name, $3, $4, $4 FROM UNNEST($1::uuid[], $2::text[]) AS t(id, name)",
            )
            .bind(&new_artist_ids)
            .bind(&new_artist_names)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to insert artists during import: {e}");
                ApiError::DatabaseError(e)
            })?;
        }

        // Songs: existing ones by (artist, title) in memory; new ones one
        // insert each (enum columns), tags in one statement at the end.
        let existing_songs: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
            "SELECT id, artist_id, LOWER(TRIM(title)) FROM songs
             WHERE user_id = $1 AND band_id IS NULL AND deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut song_by_key: HashMap<(Uuid, String), Uuid> = existing_songs
            .into_iter()
            .map(|(id, artist, title)| ((artist, title), id))
            .collect();
        let mut song_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(backup.songs.len());
        let (mut tag_song_ids, mut tag_values) = (Vec::new(), Vec::new());
        for (song, links) in backup.songs.iter().zip(song_links) {
            let Some(&resolved_artist_id) = artist_id_map.get(&song.artist_id) else {
                return Err(ApiError::BadRequest(format!(
                    "Invalid backup: the song \"{}\" references an artist that isn't in the file.",
                    song.title
                )));
            };
            let title = song.title.trim();
            let key = (resolved_artist_id, title.to_lowercase());

            let resolved_id = match song_by_key.get(&key) {
                Some(&existing_id) => existing_id,
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
                    song_by_key.insert(key, new_id);
                    new_id
                }
            };

            for tag in normalize_tags(&song.tags).unwrap_or_default() {
                tag_song_ids.push(resolved_id);
                tag_values.push(tag);
            }
            song_id_map.insert(song.id, resolved_id);
        }
        if !tag_song_ids.is_empty() {
            // A song merged into an existing one keeps the existing tags
            // and takes the file's only up to the per-song limit.
            sqlx::query(
                "INSERT INTO song_tags (song_id, tag, created_at)
                 SELECT t.song_id, t.tag, $3
                 FROM UNNEST($1::uuid[], $2::text[]) WITH ORDINALITY AS t(song_id, tag, ord)
                 WHERE t.tag NOT IN (SELECT tag FROM song_tags st WHERE st.song_id = t.song_id)
                   AND (SELECT COUNT(*) FROM song_tags st WHERE st.song_id = t.song_id)
                       + (SELECT COUNT(*) FROM UNNEST($1::uuid[], $2::text[]) WITH ORDINALITY
                             AS u(song_id, tag, ord)
                          WHERE u.song_id = t.song_id AND u.ord < t.ord
                            AND u.tag NOT IN (SELECT tag FROM song_tags st WHERE st.song_id = u.song_id))
                       < $4
                 ON CONFLICT DO NOTHING",
            )
            .bind(&tag_song_ids)
            .bind(&tag_values)
            .bind(now)
            .bind(crate::validations::tag::MAX_TAGS_PER_SONG as i64)
            .execute(&mut *tx)
            .await?;
        }

        // Setlists and all their entries: two statements.
        let mut setlist_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(backup.setlists.len());
        let mut setlist_rows = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut entries = (Vec::new(), Vec::new(), Vec::new());
        for (setlist, links) in backup.setlists.iter().zip(setlist_links) {
            let new_setlist_id = Uuid::new_v4();
            setlist_id_map.insert(setlist.id, new_setlist_id);
            setlist_rows.0.push(new_setlist_id);
            setlist_rows.1.push(setlist.title.trim().to_string());
            setlist_rows.2.push(
                setlist
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
            );
            setlist_rows.3.push(
                serde_json::to_value(&links).unwrap_or_else(|_| serde_json::Value::Array(vec![])),
            );

            // A song listed twice keeps its last position.
            let mut position_of: HashMap<Uuid, usize> = HashMap::with_capacity(setlist.songs.len());
            let mut song_ids: Vec<Uuid> = Vec::with_capacity(setlist.songs.len());
            let mut positions: Vec<i32> = Vec::with_capacity(setlist.songs.len());
            for entry in &setlist.songs {
                let Some(&resolved_song_id) = song_id_map.get(&entry.song_id) else {
                    return Err(ApiError::BadRequest(format!(
                        "Invalid backup: the setlist \"{}\" references a song that isn't in the file.",
                        setlist.title
                    )));
                };
                match position_of.get(&resolved_song_id) {
                    Some(&index) => positions[index] = entry.position,
                    None => {
                        position_of.insert(resolved_song_id, song_ids.len());
                        song_ids.push(resolved_song_id);
                        positions.push(entry.position);
                    }
                }
            }
            for (song_id, position) in song_ids.into_iter().zip(positions) {
                entries.0.push(new_setlist_id);
                entries.1.push(song_id);
                entries.2.push(position);
            }
        }
        if !setlist_rows.0.is_empty() {
            sqlx::query(
                "INSERT INTO setlists (id, title, description, user_id, links, created_at, updated_at)
                 SELECT t.id, t.title, t.description, $5, t.links, $6, $6
                 FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::jsonb[])
                      AS t(id, title, description, links)",
            )
            .bind(&setlist_rows.0)
            .bind(&setlist_rows.1)
            .bind(&setlist_rows.2)
            .bind(&setlist_rows.3)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to insert setlists during import: {e}");
                ApiError::DatabaseError(e)
            })?;
        }
        if !entries.0.is_empty() {
            sqlx::query(
                "INSERT INTO setlist_songs (setlist_id, song_id, position)
                 SELECT t.setlist_id, t.song_id, t.position
                 FROM UNNEST($1::uuid[], $2::uuid[], $3::int[]) AS t(setlist_id, song_id, position)",
            )
            .bind(&entries.0)
            .bind(&entries.1)
            .bind(&entries.2)
            .execute(&mut *tx)
            .await?;
        }

        // Tours: one statement.
        let mut tour_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(tours.len());
        let mut tour_rows = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for tour in tours {
            let new_tour_id = Uuid::new_v4();
            tour_id_map.insert(tour.id, new_tour_id);
            tour_rows.0.push(new_tour_id);
            tour_rows.1.push(tour.name.trim().to_string());
            tour_rows.2.push(
                tour.description
                    .as_deref()
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
            );
            tour_rows.3.push(tour.start_date);
            tour_rows.4.push(tour.end_date);
        }
        if !tour_rows.0.is_empty() {
            sqlx::query(
                "INSERT INTO tours (id, user_id, band_id, name, description, start_date, end_date, created_at, updated_at)
                 SELECT t.id, $6, NULL, t.name, t.description, t.start_date, t.end_date, $7, $7
                 FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::date[], $5::date[])
                      AS t(id, name, description, start_date, end_date)",
            )
            .bind(&tour_rows.0)
            .bind(&tour_rows.1)
            .bind(&tour_rows.2)
            .bind(&tour_rows.3)
            .bind(&tour_rows.4)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
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

        // The exact check, on the merged totals.
        if let Some(limits) = limits {
            let usage = usage_of(&mut *tx, user_id).await?;
            for (resource, used, limit) in [
                ("artists", usage.artists, limits.artists),
                ("songs", usage.songs, limits.songs),
                ("setlists", usage.setlists, limits.setlists),
                ("gigs", usage.gigs, limits.gigs),
                ("tags", usage.tags, limits.tags),
                ("tours", usage.tours, limits.tours),
            ] {
                if used > limit {
                    tx.rollback().await?;
                    return Err(ApiError::quota_exceeded(resource, limit));
                }
            }
        }

        tx.commit().await?;

        Ok(ImportSummary {
            artists_imported: backup.artists.len(),
            songs_imported: backup.songs.len(),
            setlists_imported: backup.setlists.len(),
            gigs_imported: backup.gigs.len(),
            tours_imported: tours.len(),
            skipped_tours,
        })
    }
}

/// `IMPORT_IN_PROGRESS` (409).
pub fn import_in_progress() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::IMPORT_IN_PROGRESS,
        "Another backup import is still running for this account. Wait for it to finish.",
    )
}

/// An account's personal content totals (what the backup quota covers).
struct Usage {
    artists: i64,
    songs: i64,
    setlists: i64,
    gigs: i64,
    tags: i64,
    tours: i64,
}

async fn usage_of<'e, E>(executor: E, user_id: Uuid) -> Result<Usage, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
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
        .fetch_one(executor)
        .await?;
    Ok(Usage {
        artists,
        songs,
        setlists,
        gigs,
        tags,
        tours,
    })
}
