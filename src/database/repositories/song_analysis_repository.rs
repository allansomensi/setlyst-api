use crate::{
    errors::api_error::{ApiError, codes},
    models::song_analysis::SongAnalysis,
};
use axum::http::StatusCode;
use chrono::{NaiveDateTime, SubsecRound, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait SongAnalysisRepository: Send + Sync {
    /// The song's analysis, if it has one. The caller must already have
    /// authorized access to the song.
    async fn find(&self, song_id: Uuid) -> Result<Option<SongAnalysis>, ApiError>;
    /// Creates or replaces the song's analysis. With `base_updated_at`, an
    /// existing analysis is only replaced while its `updated_at` is still
    /// that one (`ANALYSIS_CONFLICT`, 409, otherwise); without it the last
    /// write wins. Checked and written in one statement, so concurrent
    /// saves can't both pass the check.
    async fn save(
        &self,
        song_id: Uuid,
        content: &Value,
        base_updated_at: Option<NaiveDateTime>,
        actor_id: Uuid,
    ) -> Result<SongAnalysis, ApiError>;
    /// Removes the song's analysis (nothing to do when it has none).
    async fn delete(&self, song_id: Uuid) -> Result<(), ApiError>;
}

pub struct SongAnalysisRepositoryImpl {
    pub db: PgPool,
}

impl SongAnalysisRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

pub fn analysis_conflict(current: Option<NaiveDateTime>) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::CONFLICT,
        codes::ANALYSIS_CONFLICT,
        "The analysis was changed by someone else since you opened it.",
        json!({ "updated_at": current }),
    )
}

fn is_foreign_key_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23503"))
}

#[async_trait::async_trait]
impl SongAnalysisRepository for SongAnalysisRepositoryImpl {
    async fn find(&self, song_id: Uuid) -> Result<Option<SongAnalysis>, ApiError> {
        let row = sqlx::query_as::<_, SongAnalysis>(
            "SELECT a.song_id, a.content, a.created_at, a.updated_at,
                    u.username AS updated_by_username
             FROM song_analyses a
             LEFT JOIN users u ON u.id = a.updated_by
             WHERE a.song_id = $1",
        )
        .bind(song_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row)
    }

    async fn save(
        &self,
        song_id: Uuid,
        content: &Value,
        base_updated_at: Option<NaiveDateTime>,
        actor_id: Uuid,
    ) -> Result<SongAnalysis, ApiError> {
        // Postgres keeps microseconds: store exactly what reads return, so
        // a client echoing `updated_at` back as `base_updated_at` matches.
        let now = Utc::now().naive_utc().trunc_subsecs(6);
        // `ON CONFLICT ... DO UPDATE ... WHERE` locks the existing row and
        // checks the version against its latest state: no row comes back
        // when it has moved on.
        let saved = sqlx::query_as::<_, SongAnalysis>(
            "WITH saved AS (
                INSERT INTO song_analyses (song_id, content, created_at, updated_at, updated_by)
                VALUES ($1, $2, $3, $3, $4)
                ON CONFLICT (song_id) DO UPDATE SET
                    content = EXCLUDED.content,
                    updated_at = EXCLUDED.updated_at,
                    updated_by = EXCLUDED.updated_by
                WHERE $5::timestamp IS NULL OR song_analyses.updated_at = $5::timestamp
                RETURNING song_id, content, created_at, updated_at, updated_by
             )
             SELECT s.song_id, s.content, s.created_at, s.updated_at,
                    u.username AS updated_by_username
             FROM saved s
             LEFT JOIN users u ON u.id = s.updated_by",
        )
        .bind(song_id)
        .bind(sqlx::types::Json(content))
        .bind(now)
        .bind(actor_id)
        .bind(base_updated_at.map(|t| t.trunc_subsecs(6)))
        .fetch_optional(&self.db)
        .await
        .map_err(|e| {
            // The song was purged in the meantime.
            if is_foreign_key_violation(&e) {
                ApiError::NotFound
            } else {
                ApiError::DatabaseError(e)
            }
        })?;
        match saved {
            Some(analysis) => Ok(analysis),
            None => {
                let current = self.find(song_id).await?.map(|a| a.updated_at);
                Err(analysis_conflict(current))
            }
        }
    }

    async fn delete(&self, song_id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM song_analyses WHERE song_id = $1")
            .bind(song_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}
