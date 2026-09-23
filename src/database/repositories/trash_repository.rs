//! The trash (soft-deleted songs, artists, setlists, gigs and tours).
//!
//! Rows deleted together share a `trash_batch` (an artist and the songs
//! that went with it). Restoring the artist restores the whole batch;
//! restoring one of its songs brings back that song only (plus the artist
//! row it needs, without the artist's other songs). Restoring re-checks
//! uniqueness (`RESTORE_CONFLICT`) and quotas, since the scope may have
//! filled up in the meantime.

use crate::{
    errors::api_error::{ApiError, codes},
    models::{
        quota::QuotaLimits,
        trash::{TrashItem, TrashType},
    },
};
use axum::http::StatusCode;
use chrono::NaiveDateTime;
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Whose trash a listing or an "empty" covers.
#[derive(Debug, Clone)]
pub struct TrashScopeFilter {
    /// Personal trash of this user (when `band_id` is `None`).
    pub user_id: Uuid,
    /// A band's trash.
    pub band_id: Option<Uuid>,
    /// Kinds the caller may see/manage in this scope.
    pub types: Vec<TrashType>,
}

/// Where a trashed row belongs.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrashedRow {
    pub user_id: Uuid,
    pub band_id: Option<Uuid>,
    pub trash_batch: Option<Uuid>,
}

pub fn not_in_trash() -> ApiError {
    ApiError::rule(
        StatusCode::NOT_FOUND,
        codes::NOT_IN_TRASH,
        "This item is not in the trash.",
    )
}

fn restore_conflict(kind: TrashType) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::CONFLICT,
        codes::RESTORE_CONFLICT,
        "Something with the same name already exists. Rename it and try again.",
        json!({ "type": kind.key() }),
    )
}

#[async_trait::async_trait]
pub trait TrashRepository: Send + Sync {
    async fn list(
        &self,
        scope: &TrashScopeFilter,
        item_type: Option<TrashType>,
        retention_days: i64,
        page: i64,
        size: i64,
    ) -> Result<(Vec<TrashItem>, i64), ApiError>;
    /// Owner and band of a trashed row (`None` when missing or live).
    async fn find(&self, item_type: TrashType, id: Uuid) -> Result<Option<TrashedRow>, ApiError>;
    /// Restores the row. An artist brings its whole batch back (the songs
    /// trashed with it); a song brings back only itself and, when needed,
    /// its artist row. `limits` are the quotas of the row's scope (the
    /// user's, or the band owner's).
    async fn restore(
        &self,
        item_type: TrashType,
        id: Uuid,
        limits: Option<QuotaLimits>,
    ) -> Result<(), ApiError>;
    /// Permanently deletes a trashed row (an artist takes its trashed
    /// songs along).
    async fn delete_permanently(&self, item_type: TrashType, id: Uuid) -> Result<(), ApiError>;
    /// Permanently deletes everything in the scope. Returns the number of
    /// rows deleted.
    async fn empty(&self, scope: &TrashScopeFilter) -> Result<u64, ApiError>;
    /// Permanently deletes everything trashed before `cutoff`.
    async fn purge_before(&self, cutoff: NaiveDateTime) -> Result<u64, ApiError>;
}

pub struct TrashRepositoryImpl {
    pub db: PgPool,
}

impl TrashRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn type_keys(types: &[TrashType]) -> Vec<String> {
    types.iter().map(|t| t.key().to_string()).collect()
}

#[async_trait::async_trait]
impl TrashRepository for TrashRepositoryImpl {
    async fn list(
        &self,
        scope: &TrashScopeFilter,
        item_type: Option<TrashType>,
        retention_days: i64,
        page: i64,
        size: i64,
    ) -> Result<(Vec<TrashItem>, i64), ApiError> {
        let types: Vec<String> = match item_type {
            Some(t) if scope.types.contains(&t) => vec![t.key().to_string()],
            Some(_) => Vec::new(),
            None => type_keys(&scope.types),
        };
        let offset = (page - 1) * size;

        // One UNION over every trashable table, filtered by scope and kind.
        // Songs that went to the trash with their artist are represented
        // by the artist (see `batch_count`).
        let rows = sqlx::query_as::<_, TrashItemRow>(
            r#"
            WITH items AS (
                SELECT 'song'::text AS type, s.id, s.title,
                       (SELECT a.name FROM artists a WHERE a.id = s.artist_id) AS subtitle,
                       s.user_id, s.band_id, s.deleted_at, s.deleted_by, 0::bigint AS batch_count
                FROM songs s
                WHERE s.deleted_at IS NOT NULL
                  AND NOT EXISTS (SELECT 1 FROM artists a2
                                  WHERE a2.trash_batch = s.trash_batch AND a2.deleted_at IS NOT NULL)
                UNION ALL
                SELECT 'artist', a.id, a.name, NULL, a.user_id, a.band_id, a.deleted_at, a.deleted_by,
                       (SELECT COUNT(*) FROM songs s3
                        WHERE s3.trash_batch = a.trash_batch AND s3.deleted_at IS NOT NULL)
                FROM artists a WHERE a.deleted_at IS NOT NULL
                UNION ALL
                SELECT 'setlist', st.id, st.title, NULL, st.user_id, st.band_id, st.deleted_at, st.deleted_by, 0
                FROM setlists st WHERE st.deleted_at IS NOT NULL
                UNION ALL
                SELECT 'gig', g.id, g.venue,
                       to_char(g.scheduled_at, 'YYYY-MM-DD HH24:MI') || COALESCE(', ' || g.location, ''),
                       g.user_id, g.band_id, g.deleted_at, g.deleted_by, 0
                FROM gigs g WHERE g.deleted_at IS NOT NULL
                UNION ALL
                SELECT 'tour', t.id, t.name,
                       to_char(t.start_date, 'YYYY-MM-DD') || '..' || to_char(t.end_date, 'YYYY-MM-DD'),
                       t.user_id, t.band_id, t.deleted_at, t.deleted_by, 0
                FROM tours t WHERE t.deleted_at IS NOT NULL
            )
            SELECT i.type, i.id, i.title,
                   CASE WHEN i.type IN ('setlist', 'artist') THEN b.name ELSE i.subtitle END AS subtitle,
                   i.band_id, b.name AS band_name, i.deleted_at,
                   (SELECT u.username FROM users u WHERE u.id = i.deleted_by) AS deleted_by_username,
                   i.deleted_at + make_interval(days => $4::int) AS purge_at,
                   i.batch_count,
                   COUNT(*) OVER () AS total
            FROM items i
            LEFT JOIN bands b ON b.id = i.band_id
            WHERE i.type = ANY($3)
              AND (($2::uuid IS NOT NULL AND i.band_id = $2)
                   OR ($2::uuid IS NULL AND i.band_id IS NULL AND i.user_id = $1))
            ORDER BY i.deleted_at DESC, i.id
            LIMIT $5 OFFSET $6
            "#,
        )
        .bind(scope.user_id)
        .bind(scope.band_id)
        .bind(&types)
        .bind(retention_days.clamp(1, 3650) as i32)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db)
        .await?;

        let total = match rows.first() {
            Some(row) => row.total,
            // An empty (or past-the-end) page: count separately.
            None => {
                sqlx::query_scalar(
                    r#"
                    SELECT
                      (SELECT COUNT(*) FROM songs s WHERE s.deleted_at IS NOT NULL AND 'song' = ANY($3)
                         AND NOT EXISTS (SELECT 1 FROM artists a2 WHERE a2.trash_batch = s.trash_batch AND a2.deleted_at IS NOT NULL)
                         AND (($2::uuid IS NOT NULL AND s.band_id = $2) OR ($2::uuid IS NULL AND s.band_id IS NULL AND s.user_id = $1)))
                    + (SELECT COUNT(*) FROM artists a WHERE a.deleted_at IS NOT NULL AND 'artist' = ANY($3)
                         AND (($2::uuid IS NOT NULL AND a.band_id = $2) OR ($2::uuid IS NULL AND a.band_id IS NULL AND a.user_id = $1)))
                    + (SELECT COUNT(*) FROM setlists st WHERE st.deleted_at IS NOT NULL AND 'setlist' = ANY($3)
                         AND (($2::uuid IS NOT NULL AND st.band_id = $2) OR ($2::uuid IS NULL AND st.band_id IS NULL AND st.user_id = $1)))
                    + (SELECT COUNT(*) FROM gigs g WHERE g.deleted_at IS NOT NULL AND 'gig' = ANY($3)
                         AND (($2::uuid IS NOT NULL AND g.band_id = $2) OR ($2::uuid IS NULL AND g.band_id IS NULL AND g.user_id = $1)))
                    + (SELECT COUNT(*) FROM tours t WHERE t.deleted_at IS NOT NULL AND 'tour' = ANY($3)
                         AND (($2::uuid IS NOT NULL AND t.band_id = $2) OR ($2::uuid IS NULL AND t.band_id IS NULL AND t.user_id = $1)))
                    "#,
                )
                .bind(scope.user_id)
                .bind(scope.band_id)
                .bind(&types)
                .fetch_one(&self.db)
                .await?
            }
        };

        Ok((rows.into_iter().map(TrashItem::from).collect(), total))
    }

    async fn find(&self, item_type: TrashType, id: Uuid) -> Result<Option<TrashedRow>, ApiError> {
        let query = match item_type {
            TrashType::Song => {
                "SELECT user_id, band_id, trash_batch FROM songs WHERE id = $1 AND deleted_at IS NOT NULL"
            }
            TrashType::Artist => {
                "SELECT user_id, band_id, trash_batch FROM artists WHERE id = $1 AND deleted_at IS NOT NULL"
            }
            TrashType::Setlist => {
                "SELECT user_id, band_id, trash_batch FROM setlists WHERE id = $1 AND deleted_at IS NOT NULL"
            }
            TrashType::Gig => {
                "SELECT user_id, band_id, trash_batch FROM gigs WHERE id = $1 AND deleted_at IS NOT NULL"
            }
            TrashType::Tour => {
                "SELECT user_id, band_id, trash_batch FROM tours WHERE id = $1 AND deleted_at IS NOT NULL"
            }
        };
        let row = sqlx::query_as::<_, TrashedRow>(query)
            .bind(id)
            .fetch_optional(&self.db)
            .await?;
        Ok(row)
    }

    async fn restore(
        &self,
        item_type: TrashType,
        id: Uuid,
        limits: Option<QuotaLimits>,
    ) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;

        let row = lock_trashed(&mut tx, item_type, id).await?;
        // Only an artist restores its batch: a song trashed along with its
        // artist is restored on its own (the person picked that one song,
        // not the whole artist). Rows trashed before batches existed have
        // none and are restored alone too.
        let batch = match item_type {
            TrashType::Artist => row.trash_batch,
            _ => None,
        };

        let songs: Vec<(Uuid, Uuid, Option<Uuid>, Option<Uuid>, String, Uuid)> = sqlx::query_as(
            "SELECT id, user_id, band_id, forked_from, title, artist_id FROM songs
             WHERE deleted_at IS NOT NULL
               AND (($1::uuid IS NOT NULL AND trash_batch = $1) OR ($3 = 'song' AND id = $2))",
        )
        .bind(batch)
        .bind(id)
        .bind(item_type.key())
        .fetch_all(&mut *tx)
        .await?;

        // Restoring a song whose artist is still in the trash brings that
        // artist back too (the artist row only, not its other songs).
        let mut artist_ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artists
             WHERE deleted_at IS NOT NULL
               AND (($1::uuid IS NOT NULL AND trash_batch = $1) OR ($3 = 'artist' AND id = $2))",
        )
        .bind(batch)
        .bind(id)
        .bind(item_type.key())
        .fetch_all(&mut *tx)
        .await?;
        for song in &songs {
            let artist_deleted: bool =
                sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM artists WHERE id = $1")
                    .bind(song.5)
                    .fetch_one(&mut *tx)
                    .await?;
            if artist_deleted && !artist_ids.contains(&song.5) {
                artist_ids.push(song.5);
            }
        }

        // Uniqueness against live rows.
        for artist_id in &artist_ids {
            let conflict: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM artists a, artists t
                    WHERE t.id = $1 AND a.deleted_at IS NULL AND a.id <> t.id
                      AND LOWER(a.name) = LOWER(t.name)
                      AND ((t.band_id IS NULL AND a.band_id IS NULL AND a.user_id = t.user_id)
                           OR a.band_id = t.band_id))",
            )
            .bind(artist_id)
            .fetch_one(&mut *tx)
            .await?;
            if conflict {
                return Err(restore_conflict(TrashType::Artist));
            }
        }
        for (song_id, ..) in &songs {
            let conflict: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM songs s, songs t
                    WHERE t.id = $1 AND s.deleted_at IS NULL AND s.id <> t.id
                      AND ((t.band_id IS NULL AND s.band_id IS NULL AND s.user_id = t.user_id
                            AND s.artist_id = t.artist_id
                            AND LOWER(TRIM(s.title)) = LOWER(TRIM(t.title)))
                           OR (t.band_id IS NOT NULL AND t.forked_from IS NOT NULL
                               AND s.band_id = t.band_id AND s.forked_from = t.forked_from)))",
            )
            .bind(song_id)
            .fetch_one(&mut *tx)
            .await?;
            if conflict {
                return Err(restore_conflict(TrashType::Song));
            }
        }
        if item_type == TrashType::Setlist {
            let conflict: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM setlists s, setlists t
                    WHERE t.id = $1 AND s.deleted_at IS NULL AND s.id <> t.id AND NOT s.is_repertoire
                      AND s.title = t.title
                      AND ((t.band_id IS NULL AND s.band_id IS NULL AND s.user_id = t.user_id)
                           OR s.band_id = t.band_id))",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            if conflict {
                return Err(restore_conflict(TrashType::Setlist));
            }
        }

        // Quotas of the scope.
        if let Some(limits) = limits {
            check_restore_quotas(
                &mut tx,
                &row,
                &limits,
                songs.len() as i64,
                artist_ids.len() as i64,
                item_type,
            )
            .await?;
        }

        sqlx::query(
            "UPDATE artists SET deleted_at = NULL, deleted_by = NULL, trash_batch = NULL
             WHERE id = ANY($1)",
        )
        .bind(&artist_ids)
        .execute(&mut *tx)
        .await?;
        let song_ids: Vec<Uuid> = songs.iter().map(|s| s.0).collect();
        sqlx::query(
            "UPDATE songs SET deleted_at = NULL, deleted_by = NULL, trash_batch = NULL
             WHERE id = ANY($1)",
        )
        .bind(&song_ids)
        .execute(&mut *tx)
        .await?;
        let other = match item_type {
            TrashType::Setlist => Some(
                "UPDATE setlists SET deleted_at = NULL, deleted_by = NULL, trash_batch = NULL WHERE id = $1",
            ),
            TrashType::Gig => Some(
                "UPDATE gigs SET deleted_at = NULL, deleted_by = NULL, trash_batch = NULL WHERE id = $1",
            ),
            TrashType::Tour => Some(
                "UPDATE tours SET deleted_at = NULL, deleted_by = NULL, trash_batch = NULL WHERE id = $1",
            ),
            TrashType::Song | TrashType::Artist => None,
        };
        if let Some(query) = other {
            sqlx::query(query).bind(id).execute(&mut *tx).await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn delete_permanently(&self, item_type: TrashType, id: Uuid) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let row = lock_trashed(&mut tx, item_type, id).await?;

        let deleted = match item_type {
            TrashType::Song => {
                sqlx::query("DELETE FROM songs WHERE id = $1 AND deleted_at IS NOT NULL")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
            TrashType::Artist => {
                if let Some(batch) = row.trash_batch {
                    sqlx::query(
                        "DELETE FROM songs WHERE trash_batch = $1 AND deleted_at IS NOT NULL",
                    )
                    .bind(batch)
                    .execute(&mut *tx)
                    .await?;
                }
                // Never takes live songs down through the foreign key.
                sqlx::query(
                    "DELETE FROM artists a WHERE a.id = $1 AND a.deleted_at IS NOT NULL
                       AND NOT EXISTS (SELECT 1 FROM songs s WHERE s.artist_id = a.id AND s.deleted_at IS NULL)",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
            }
            TrashType::Setlist => sqlx::query(
                "DELETE FROM setlists WHERE id = $1 AND deleted_at IS NOT NULL AND NOT is_repertoire",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected(),
            TrashType::Gig => {
                sqlx::query("DELETE FROM gigs WHERE id = $1 AND deleted_at IS NOT NULL")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
            TrashType::Tour => {
                sqlx::query("DELETE FROM tours WHERE id = $1 AND deleted_at IS NOT NULL")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
        };
        if deleted == 0 {
            return Err(not_in_trash());
        }
        tx.commit().await?;
        Ok(())
    }

    async fn empty(&self, scope: &TrashScopeFilter) -> Result<u64, ApiError> {
        let types = type_keys(&scope.types);
        let mut tx = self.db.begin().await?;
        let mut total = 0u64;
        for query in [
            "DELETE FROM songs WHERE deleted_at IS NOT NULL AND 'song' = ANY($3)
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))",
            "DELETE FROM artists a WHERE a.deleted_at IS NOT NULL AND 'artist' = ANY($3)
               AND (($2::uuid IS NOT NULL AND a.band_id = $2) OR ($2::uuid IS NULL AND a.band_id IS NULL AND a.user_id = $1))
               AND NOT EXISTS (SELECT 1 FROM songs s WHERE s.artist_id = a.id AND s.deleted_at IS NULL)",
            "DELETE FROM setlists WHERE deleted_at IS NOT NULL AND NOT is_repertoire AND 'setlist' = ANY($3)
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))",
            "DELETE FROM gigs WHERE deleted_at IS NOT NULL AND 'gig' = ANY($3)
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))",
            "DELETE FROM tours WHERE deleted_at IS NOT NULL AND 'tour' = ANY($3)
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))",
        ] {
            total += sqlx::query(query)
                .bind(scope.user_id)
                .bind(scope.band_id)
                .bind(&types)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }

    async fn purge_before(&self, cutoff: NaiveDateTime) -> Result<u64, ApiError> {
        let mut total = 0u64;
        // Songs before artists (an artist is only purged once no live
        // song refers to it); each statement commits on its own so a huge
        // backlog doesn't hold one long transaction.
        for query in [
            "DELETE FROM songs WHERE deleted_at < $1",
            "DELETE FROM artists a WHERE a.deleted_at < $1
               AND NOT EXISTS (SELECT 1 FROM songs s WHERE s.artist_id = a.id AND s.deleted_at IS NULL)",
            "DELETE FROM setlists WHERE deleted_at < $1 AND NOT is_repertoire",
            "DELETE FROM gigs WHERE deleted_at < $1",
            "DELETE FROM tours WHERE deleted_at < $1",
        ] {
            total += sqlx::query(query)
                .bind(cutoff)
                .execute(&self.db)
                .await?
                .rows_affected();
        }
        Ok(total)
    }
}

#[derive(sqlx::FromRow)]
struct TrashItemRow {
    #[sqlx(rename = "type")]
    item_type: String,
    id: Uuid,
    title: String,
    subtitle: Option<String>,
    band_id: Option<Uuid>,
    band_name: Option<String>,
    deleted_at: NaiveDateTime,
    deleted_by_username: Option<String>,
    purge_at: NaiveDateTime,
    batch_count: i64,
    total: i64,
}

impl From<TrashItemRow> for TrashItem {
    fn from(row: TrashItemRow) -> Self {
        Self {
            item_type: row.item_type,
            id: row.id,
            title: row.title,
            subtitle: row.subtitle,
            band_id: row.band_id,
            band_name: row.band_name,
            deleted_at: row.deleted_at,
            deleted_by_username: row.deleted_by_username,
            purge_at: row.purge_at,
            batch_count: row.batch_count,
        }
    }
}

/// Locks a trashed row for the rest of the transaction.
async fn lock_trashed(
    tx: &mut Transaction<'_, Postgres>,
    item_type: TrashType,
    id: Uuid,
) -> Result<TrashedRow, ApiError> {
    let query = match item_type {
        TrashType::Song => {
            "SELECT user_id, band_id, trash_batch FROM songs WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE"
        }
        TrashType::Artist => {
            "SELECT user_id, band_id, trash_batch FROM artists WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE"
        }
        TrashType::Setlist => {
            "SELECT user_id, band_id, trash_batch FROM setlists WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE"
        }
        TrashType::Gig => {
            "SELECT user_id, band_id, trash_batch FROM gigs WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE"
        }
        TrashType::Tour => {
            "SELECT user_id, band_id, trash_batch FROM tours WHERE id = $1 AND deleted_at IS NOT NULL FOR UPDATE"
        }
    };
    sqlx::query_as::<_, TrashedRow>(query)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(not_in_trash)
}

/// Fails with `QUOTA_EXCEEDED` when bringing the rows back would take the
/// scope over a limit.
async fn check_restore_quotas(
    tx: &mut Transaction<'_, Postgres>,
    row: &TrashedRow,
    limits: &QuotaLimits,
    songs: i64,
    artists: i64,
    item_type: TrashType,
) -> Result<(), ApiError> {
    let (setlists, gigs, tours) = match item_type {
        TrashType::Setlist => (1, 0, 0),
        TrashType::Gig => (0, 1, 0),
        TrashType::Tour => (0, 0, 1),
        _ => (0, 0, 0),
    };

    let (used_songs, used_artists, used_setlists, used_gigs, used_tours): (
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM songs WHERE deleted_at IS NULL
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))),
            (SELECT COUNT(*) FROM artists WHERE deleted_at IS NULL
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))),
            (SELECT COUNT(*) FROM setlists WHERE deleted_at IS NULL
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))),
            (SELECT COUNT(*) FROM gigs WHERE deleted_at IS NULL
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1))),
            (SELECT COUNT(*) FROM tours WHERE deleted_at IS NULL
               AND (($2::uuid IS NOT NULL AND band_id = $2) OR ($2::uuid IS NULL AND band_id IS NULL AND user_id = $1)))",
    )
    .bind(row.user_id)
    .bind(row.band_id)
    .fetch_one(&mut **tx)
    .await?;

    let checks: Vec<(&str, i64, i64, i64)> = match row.band_id {
        None => vec![
            ("songs", used_songs, songs, limits.songs),
            ("artists", used_artists, artists, limits.artists),
            ("setlists", used_setlists, setlists, limits.setlists),
            ("gigs", used_gigs, gigs, limits.gigs),
            ("tours", used_tours, tours, limits.tours),
        ],
        Some(_) => vec![
            ("band_songs", used_songs, songs, limits.band_songs),
            (
                "band_setlists",
                used_setlists,
                setlists,
                limits.band_setlists,
            ),
            ("band_gigs", used_gigs, gigs, limits.band_gigs),
            ("band_tours", used_tours, tours, limits.band_tours),
        ],
    };
    for (resource, used, adding, limit) in checks {
        if adding > 0 && used + adding > limit {
            return Err(ApiError::quota_exceeded(resource, limit));
        }
    }
    Ok(())
}
