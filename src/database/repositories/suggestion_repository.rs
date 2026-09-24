use crate::{
    errors::api_error::{ApiError, codes},
    models::suggestion::{Suggestion, SuggestionRow, SuggestionStatus},
};
use axum::http::StatusCode;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

/// Columns of a [`SuggestionRow`] from `band_song_suggestions bs`; `$2`
/// is the caller (for `my_vote`).
macro_rules! suggestion_select {
    () => {
        "SELECT bs.id, bs.band_id, bs.setlist_id, st.title AS setlist_title,
                st.is_repertoire AS setlist_is_repertoire,
                bs.song_id, so.id AS live_song_id, so.title AS live_song_title,
                a.name AS live_artist_name, so.tonality AS live_tonality, so.tempo AS live_tempo,
                so.energy AS live_energy, so.duration AS live_duration, so.links AS live_links,
                so.band_id AS live_song_band_id,
                bs.song_title, bs.artist_name, bs.suggested_by,
                u.username AS suggested_by_username, u.avatar_url AS suggested_by_avatar_url,
                bs.note, bs.status,
                (SELECT COUNT(*) FROM band_song_suggestion_votes v
                    WHERE v.suggestion_id = bs.id AND v.value = 1) AS votes_up,
                (SELECT COUNT(*) FROM band_song_suggestion_votes v
                    WHERE v.suggestion_id = bs.id AND v.value = -1) AS votes_down,
                COALESCE((SELECT v.value FROM band_song_suggestion_votes v
                    WHERE v.suggestion_id = bs.id AND v.user_id = $2), 0::smallint) AS my_vote,
                (SELECT r.username FROM users r WHERE r.id = bs.resolved_by) AS resolved_by_username,
                bs.resolved_at, bs.resolution_note, bs.created_at
         FROM band_song_suggestions bs
         INNER JOIN setlists st ON st.id = bs.setlist_id
         LEFT JOIN songs so ON so.id = bs.song_id AND so.deleted_at IS NULL
         LEFT JOIN artists a ON a.id = so.artist_id
         LEFT JOIN users u ON u.id = bs.suggested_by"
    };
}

pub fn suggestion_closed() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::SUGGESTION_CLOSED,
        "This suggestion is no longer open.",
    )
}

/// What a new suggestion records.
pub struct NewSuggestion<'a> {
    pub band_id: Uuid,
    pub setlist_id: Uuid,
    pub song_id: Uuid,
    pub song_title: &'a str,
    pub artist_name: &'a str,
    pub suggested_by: Uuid,
    pub note: Option<&'a str>,
}

#[async_trait::async_trait]
pub trait SuggestionRepository: Send + Sync {
    async fn list(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        status: Option<SuggestionStatus>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Suggestion>, i64), ApiError>;
    /// A suggestion of `band_id`, as seen by `user_id`.
    async fn find(
        &self,
        id: Uuid,
        band_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SuggestionRow>, ApiError>;
    /// Creates it with the suggester's own up-vote. `ALREADY_EXISTS` when
    /// the same song is already open for the same setlist.
    async fn create(&self, new: &NewSuggestion<'_>) -> Result<Uuid, ApiError>;
    /// Casts or changes a vote on an open suggestion (`SUGGESTION_CLOSED`
    /// otherwise). Returns the `(up, down)` counts that decide automatic
    /// acceptance: votes of current members other than the suggester.
    async fn vote(&self, id: Uuid, user_id: Uuid, value: i16) -> Result<(i64, i64), ApiError>;
    async fn remove_vote(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// Closes an open suggestion with `status` (`SUGGESTION_CLOSED` when it
    /// was already closed).
    async fn resolve(
        &self,
        id: Uuid,
        status: SuggestionStatus,
        resolved_by: Option<Uuid>,
        note: Option<&str>,
    ) -> Result<(), ApiError>;
    /// Undoes an acceptance whose effects could not be applied: back to
    /// `open`, as if it had never been claimed.
    async fn reopen(&self, id: Uuid) -> Result<(), ApiError>;
}

pub struct SuggestionRepositoryImpl {
    pub db: PgPool,
}

impl SuggestionRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl SuggestionRepository for SuggestionRepositoryImpl {
    async fn list(
        &self,
        band_id: Uuid,
        user_id: Uuid,
        status: Option<SuggestionStatus>,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Suggestion>, i64), ApiError> {
        let offset = (page - 1) * size;
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM band_song_suggestions
             WHERE band_id = $1 AND ($2::suggestion_status IS NULL OR status = $2)",
        )
        .bind(band_id)
        .bind(status)
        .fetch_one(&self.db);

        let rows = sqlx::query_as::<_, SuggestionRow>(concat!(
            suggestion_select!(),
            " WHERE bs.band_id = $1 AND ($3::suggestion_status IS NULL OR bs.status = $3)
             ORDER BY bs.created_at DESC, bs.id
             LIMIT $4 OFFSET $5"
        ))
        .bind(band_id)
        .bind(user_id)
        .bind(status)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, rows) = tokio::try_join!(count, rows)?;
        Ok((rows.into_iter().map(Suggestion::from).collect(), count))
    }

    async fn find(
        &self,
        id: Uuid,
        band_id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<SuggestionRow>, ApiError> {
        let row = sqlx::query_as::<_, SuggestionRow>(concat!(
            suggestion_select!(),
            " WHERE bs.id = $1 AND bs.band_id = $3"
        ))
        .bind(id)
        .bind(user_id)
        .bind(band_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row)
    }

    async fn create(&self, new: &NewSuggestion<'_>) -> Result<Uuid, ApiError> {
        let id = Uuid::new_v4();
        let now = Utc::now().naive_utc();
        let mut tx = self.db.begin().await?;
        sqlx::query(
            "INSERT INTO band_song_suggestions
                (id, band_id, setlist_id, song_id, song_title, artist_name, suggested_by, note, status, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'open', $9, $9)",
        )
        .bind(id)
        .bind(new.band_id)
        .bind(new.setlist_id)
        .bind(new.song_id)
        .bind(new.song_title)
        .bind(new.artist_name)
        .bind(new.suggested_by)
        .bind(new.note)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
                ApiError::AlreadyExists
            }
            _ => ApiError::DatabaseError(e),
        })?;
        sqlx::query(
            "INSERT INTO band_song_suggestion_votes (suggestion_id, user_id, value, created_at, updated_at)
             VALUES ($1, $2, 1, $3, $3)",
        )
        .bind(id)
        .bind(new.suggested_by)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    async fn vote(&self, id: Uuid, user_id: Uuid, value: i16) -> Result<(i64, i64), ApiError> {
        let now = Utc::now().naive_utc();
        let mut tx = self.db.begin().await?;
        let status: SuggestionStatus =
            sqlx::query_scalar("SELECT status FROM band_song_suggestions WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ApiError::NotFound)?;
        if status != SuggestionStatus::Open {
            return Err(suggestion_closed());
        }
        sqlx::query(
            "INSERT INTO band_song_suggestion_votes (suggestion_id, user_id, value, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             ON CONFLICT (suggestion_id, user_id) DO UPDATE SET value = $3, updated_at = $4",
        )
        .bind(id)
        .bind(user_id)
        .bind(value)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // The votes that decide automatic acceptance: current members
        // other than the suggester (whose own up-vote would otherwise let
        // a threshold of 1 accept anything a member suggests).
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*) FILTER (WHERE v.value = 1), COUNT(*) FILTER (WHERE v.value = -1)
             FROM band_song_suggestion_votes v
             INNER JOIN band_song_suggestions bs ON bs.id = v.suggestion_id
             INNER JOIN band_members m ON m.band_id = bs.band_id AND m.user_id = v.user_id
             WHERE v.suggestion_id = $1 AND v.user_id IS DISTINCT FROM bs.suggested_by",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(counts)
    }

    async fn remove_vote(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        let status: SuggestionStatus =
            sqlx::query_scalar("SELECT status FROM band_song_suggestions WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ApiError::NotFound)?;
        if status != SuggestionStatus::Open {
            return Err(suggestion_closed());
        }
        sqlx::query(
            "DELETE FROM band_song_suggestion_votes WHERE suggestion_id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn resolve(
        &self,
        id: Uuid,
        status: SuggestionStatus,
        resolved_by: Option<Uuid>,
        note: Option<&str>,
    ) -> Result<(), ApiError> {
        let now = Utc::now().naive_utc();
        let result = sqlx::query(
            "UPDATE band_song_suggestions
             SET status = $2, resolved_by = $3, resolved_at = $4, resolution_note = $5, updated_at = $4
             WHERE id = $1 AND status = 'open'",
        )
        .bind(id)
        .bind(status)
        .bind(resolved_by)
        .bind(now)
        .bind(note)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(suggestion_closed());
        }
        Ok(())
    }

    async fn reopen(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query(
            "UPDATE band_song_suggestions
             SET status = 'open', resolved_by = NULL, resolved_at = NULL, resolution_note = NULL,
                 updated_at = $2
             WHERE id = $1 AND status = 'accepted'",
        )
        .bind(id)
        .bind(Utc::now().naive_utc())
        .execute(&self.db)
        .await?;
        Ok(())
    }
}
