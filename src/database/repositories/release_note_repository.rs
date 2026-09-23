use crate::{
    errors::api_error::ApiError,
    models::release_note::{
        CreateReleaseNotePayload, ReleaseNote, ReleaseNoteRow, UpdateReleaseNotePayload, trim_items,
    },
};
use chrono::{NaiveDateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::announcement_repository::EmailRecipient;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

macro_rules! release_columns {
    () => {
        "r.id, r.version, r.title, r.items, r.released_on, r.published_at, r.created_at, r.updated_at,
         (SELECT x.username FROM users x WHERE x.id = r.updated_by) AS updated_by_username"
    };
}

/// Accounts that receive platform-wide messages (active, not suspended).
macro_rules! reachable_users {
    () => {
        "FROM users u LEFT JOIN user_preferences up ON up.user_id = u.id
         WHERE u.status = 'active'
           AND NOT (u.banned_at IS NOT NULL AND (u.banned_until IS NULL OR u.banned_until > (NOW() AT TIME ZONE 'utc')))"
    };
}

#[async_trait::async_trait]
pub trait ReleaseNoteRepository: Send + Sync {
    /// Published notes, newest `released_on` first (at most `limit`).
    async fn list_published(&self, limit: i64) -> Result<Vec<ReleaseNote>, ApiError>;
    /// Every note, drafts included.
    async fn list_all(&self) -> Result<Vec<ReleaseNote>, ApiError>;
    async fn find(&self, id: Uuid) -> Result<Option<ReleaseNote>, ApiError>;
    async fn create(
        &self,
        payload: &CreateReleaseNotePayload,
        actor_id: Uuid,
    ) -> Result<ReleaseNote, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateReleaseNotePayload,
        actor_id: Uuid,
    ) -> Result<Option<ReleaseNote>, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<bool, ApiError>;
    /// Sets (or clears, with `false`) the publication timestamp. Returns
    /// whether the note was a draft before.
    async fn set_published(
        &self,
        id: Uuid,
        published: bool,
        actor_id: Uuid,
    ) -> Result<bool, ApiError>;
    /// Creates a `release_published` notification for every reachable
    /// account whose `product_updates.in_app` preference allows it, and
    /// returns the verified addresses that opted in to product-update
    /// e-mails.
    async fn fan_out(&self, note: &ReleaseNote) -> Result<(u64, Vec<EmailRecipient>), ApiError>;
}

pub struct ReleaseNoteRepositoryImpl {
    pub db: PgPool,
}

impl ReleaseNoteRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl ReleaseNoteRepository for ReleaseNoteRepositoryImpl {
    async fn list_published(&self, limit: i64) -> Result<Vec<ReleaseNote>, ApiError> {
        let rows = sqlx::query_as::<_, ReleaseNoteRow>(concat!(
            "SELECT ",
            release_columns!(),
            " FROM release_notes r WHERE r.published_at IS NOT NULL
              ORDER BY r.released_on DESC, r.created_at DESC LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(ReleaseNote::from).collect())
    }

    async fn list_all(&self) -> Result<Vec<ReleaseNote>, ApiError> {
        let rows = sqlx::query_as::<_, ReleaseNoteRow>(concat!(
            "SELECT ",
            release_columns!(),
            " FROM release_notes r ORDER BY r.released_on DESC, r.created_at DESC LIMIT 500"
        ))
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(ReleaseNote::from).collect())
    }

    async fn find(&self, id: Uuid) -> Result<Option<ReleaseNote>, ApiError> {
        let row = sqlx::query_as::<_, ReleaseNoteRow>(concat!(
            "SELECT ",
            release_columns!(),
            " FROM release_notes r WHERE r.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(ReleaseNote::from))
    }

    async fn create(
        &self,
        payload: &CreateReleaseNotePayload,
        actor_id: Uuid,
    ) -> Result<ReleaseNote, ApiError> {
        let id = Uuid::now_v7();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO release_notes (id, version, title, items, released_on, created_by, updated_by,
                                        created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $7)",
        )
        .bind(id)
        .bind(payload.version.trim())
        .bind(crate::models::billing::trim_localized(&payload.title))
        .bind(serde_json::to_value(trim_items(&payload.items)).unwrap_or_default())
        .bind(payload.released_on)
        .bind(actor_id)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateReleaseNotePayload,
        actor_id: Uuid,
    ) -> Result<Option<ReleaseNote>, ApiError> {
        let result = sqlx::query(
            "UPDATE release_notes SET
                 version = COALESCE($2, version),
                 title = COALESCE($3, title),
                 items = COALESCE($4, items),
                 released_on = COALESCE($5, released_on),
                 updated_by = $6, updated_at = $7
             WHERE id = $1",
        )
        .bind(id)
        .bind(payload.version.as_deref().map(str::trim))
        .bind(
            payload
                .title
                .as_ref()
                .map(crate::models::billing::trim_localized),
        )
        .bind(
            payload
                .items
                .as_ref()
                .map(|items| serde_json::to_value(trim_items(items)).unwrap_or_default()),
        )
        .bind(payload.released_on)
        .bind(actor_id)
        .bind(now())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        self.find(id).await
    }

    async fn delete(&self, id: Uuid) -> Result<bool, ApiError> {
        let result = sqlx::query("DELETE FROM release_notes WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_published(
        &self,
        id: Uuid,
        published: bool,
        actor_id: Uuid,
    ) -> Result<bool, ApiError> {
        let timestamp = now();
        // `updated_at` moves with the publication so a fresh publication
        // doesn't count as an edit.
        let was_draft: Option<bool> = sqlx::query_scalar(
            "UPDATE release_notes r SET
                 published_at = CASE WHEN $2 THEN COALESCE(r.published_at, $3) ELSE NULL END,
                 updated_at = CASE WHEN $2 AND r.published_at IS NULL THEN $3 ELSE r.updated_at END,
                 updated_by = $4
             FROM (SELECT published_at FROM release_notes WHERE id = $1) AS old
             WHERE r.id = $1
             RETURNING old.published_at IS NULL",
        )
        .bind(id)
        .bind(published)
        .bind(timestamp)
        .bind(actor_id)
        .fetch_optional(&self.db)
        .await?;
        was_draft.ok_or(ApiError::NotFound)
    }

    async fn fan_out(&self, note: &ReleaseNote) -> Result<(u64, Vec<EmailRecipient>), ApiError> {
        let created = sqlx::query(concat!(
            "INSERT INTO notifications (id, user_id, type, data, read_at, created_at)
             SELECT gen_random_uuid(), u.id, 'release_published',
                    jsonb_build_object('version', $1::text, 'release_id', $2::uuid), NULL, $3 ",
            reachable_users!(),
            " AND COALESCE((up.communication->'categories'->'product_updates'->>'in_app')::boolean, TRUE)"
        ))
        .bind(&note.version)
        .bind(note.id)
        .bind(now())
        .execute(&self.db)
        .await?
        .rows_affected();

        let recipients = sqlx::query_as::<_, EmailRecipient>(concat!(
            "SELECT u.id AS user_id, u.email, COALESCE(up.language, 'en') AS language ",
            reachable_users!(),
            " AND u.email IS NOT NULL AND u.email_verified_at IS NOT NULL
              AND COALESCE((up.communication->'categories'->'product_updates'->>'email')::boolean, FALSE)"
        ))
        .fetch_all(&self.db)
        .await?;

        Ok((created, recipients))
    }
}
