use crate::{
    errors::api_error::ApiError,
    models::backup::{
        BACKUP_FORMAT_VERSION, BackupArtist, BackupFile, BackupSetlist, BackupSetlistSong,
        BackupSong, ImportSummary,
    },
};
use chrono::Utc;
use sqlx::PgPool;
use std::collections::HashMap;
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
}

#[derive(sqlx::FromRow)]
struct SetlistRow {
    id: Uuid,
    title: String,
    description: Option<String>,
}

#[derive(sqlx::FromRow)]
struct SetlistSongRow {
    setlist_id: Uuid,
    song_id: Uuid,
    position: i32,
}

#[async_trait::async_trait]
pub trait BackupRepository: Send + Sync {
    /// Collects all of a user's artists, songs and setlists and returns them
    /// as a portable, self-contained [`BackupFile`].
    async fn export(&self, user_id: Uuid) -> Result<BackupFile, ApiError>;

    /// Atomically imports a [`BackupFile`] into the target user's account.
    ///
    /// **Merge rules:**
    /// - Artists with the same name that already exist are reused, not duplicated.
    /// - Songs with the same (title, artist, user) that already exist are reused.
    /// - Setlists are always created as new entries (same title is intentionally
    ///   allowed — a setlist is a specific ordered list, not a unique name).
    /// - All IDs are remapped to fresh UUIDs; the backup IDs are only used as
    ///   reference keys during the import phase.
    ///
    /// The entire operation is wrapped in a single database transaction; any
    /// failure rolls back completely, leaving the target account untouched.
    async fn import(&self, user_id: Uuid, backup: BackupFile) -> Result<ImportSummary, ApiError>;
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
            "SELECT id, name FROM artists WHERE user_id = $1 ORDER BY name ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let songs_fut = sqlx::query_as::<_, SongRow>(
            "SELECT id, title, artist_id, tempo, lyrics, tonality, genre, duration
             FROM songs
             WHERE user_id = $1
             ORDER BY title ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let setlists_fut = sqlx::query_as::<_, SetlistRow>(
            "SELECT id, title, description FROM setlists WHERE user_id = $1 ORDER BY title ASC",
        )
        .bind(user_id)
        .fetch_all(&self.db);

        let (artist_rows, song_rows, setlist_rows) =
            tokio::try_join!(artists_fut, songs_fut, setlists_fut)?;

        let setlist_ids: Vec<Uuid> = setlist_rows.iter().map(|s| s.id).collect();

        let setlist_song_rows: Vec<SetlistSongRow> = if setlist_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as::<_, SetlistSongRow>(
                "SELECT setlist_id, song_id, position
                 FROM setlist_songs
                 WHERE setlist_id = ANY($1)
                 ORDER BY setlist_id, position ASC",
            )
            .bind(&setlist_ids)
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
            })
            .collect();

        let setlists = setlist_rows
            .into_iter()
            .map(|r| BackupSetlist {
                id: r.id,
                title: r.title,
                description: r.description,
                songs: songs_by_setlist.remove(&r.id).unwrap_or_default(),
            })
            .collect();

        Ok(BackupFile {
            version: BACKUP_FORMAT_VERSION,
            exported_at: Utc::now().naive_utc(),
            artists,
            songs,
            setlists,
        })
    }

    async fn import(&self, user_id: Uuid, backup: BackupFile) -> Result<ImportSummary, ApiError> {
        let now = Utc::now().naive_utc();

        let artists_incoming = backup.artists.len();
        let songs_incoming = backup.songs.len();
        let setlists_incoming = backup.setlists.len();

        let mut tx = self.db.begin().await?;

        let mut artist_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(artists_incoming);

        for artist in &backup.artists {
            let resolved_id: Uuid =
                match sqlx::query_scalar("SELECT id FROM artists WHERE name = $1 AND user_id = $2")
                    .bind(&artist.name)
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
                        .bind(&artist.name)
                        .bind(user_id)
                        .bind(now)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| {
                            error!("Failed to insert artist '{}': {e}", artist.name);
                            ApiError::DatabaseError(e)
                        })?;
                        new_id
                    }
                };

            artist_id_map.insert(artist.id, resolved_id);
        }

        let mut song_id_map: HashMap<Uuid, Uuid> = HashMap::with_capacity(songs_incoming);

        for song in &backup.songs {
            let resolved_artist_id = match artist_id_map.get(&song.artist_id) {
                Some(&id) => id,
                None => {
                    error!(
                        "Song '{}' references unknown artist ID {} — aborting import",
                        song.title, song.artist_id
                    );
                    tx.rollback().await?;
                    return Err(ApiError::DatabaseError(sqlx::Error::RowNotFound));
                }
            };

            let resolved_id: Uuid = match sqlx::query_scalar(
                "SELECT id FROM songs WHERE title = $1 AND artist_id = $2 AND user_id = $3",
            )
            .bind(&song.title)
            .bind(resolved_artist_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                Some(existing_id) => existing_id,
                None => {
                    let new_id = Uuid::new_v4();
                    sqlx::query(
                        "INSERT INTO songs
                         (id, title, artist_id, user_id, tempo, lyrics, tonality, genre, duration,
                          created_at, updated_at)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
                    )
                    .bind(new_id)
                    .bind(&song.title)
                    .bind(resolved_artist_id)
                    .bind(user_id)
                    .bind(song.tempo)
                    .bind(&song.lyrics)
                    .bind(song.tonality)
                    .bind(song.genre)
                    .bind(song.duration)
                    .bind(now)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        error!("Failed to insert song '{}': {e}", song.title);
                        ApiError::DatabaseError(e)
                    })?;
                    new_id
                }
            };

            song_id_map.insert(song.id, resolved_id);
        }

        for setlist in &backup.setlists {
            let new_setlist_id = Uuid::new_v4();

            sqlx::query(
                "INSERT INTO setlists (id, title, description, user_id, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $5)",
            )
            .bind(new_setlist_id)
            .bind(&setlist.title)
            .bind(&setlist.description)
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                error!("Failed to insert setlist '{}': {e}", setlist.title);
                ApiError::DatabaseError(e)
            })?;

            for entry in &setlist.songs {
                let resolved_song_id = match song_id_map.get(&entry.song_id) {
                    Some(&id) => id,
                    None => {
                        error!(
                            "Setlist '{}' references unknown song ID {} — aborting import",
                            setlist.title, entry.song_id
                        );
                        tx.rollback().await?;
                        return Err(ApiError::DatabaseError(sqlx::Error::RowNotFound));
                    }
                };

                sqlx::query(
                    "INSERT INTO setlist_songs (setlist_id, song_id, position)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (setlist_id, song_id)
                     DO UPDATE SET position = EXCLUDED.position",
                )
                .bind(new_setlist_id)
                .bind(resolved_song_id)
                .bind(entry.position)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    error!("Failed to insert setlist_song entry: {e}");
                    ApiError::DatabaseError(e)
                })?;
            }
        }

        tx.commit().await?;

        Ok(ImportSummary {
            artists_imported: artists_incoming,
            songs_imported: songs_incoming,
            setlists_imported: setlists_incoming,
        })
    }
}
