use crate::{
    errors::api_error::ApiError,
    models::band_note::{BandNoteColor, BandNoteRow, CreateBandNotePayload, UpdateBandNotePayload},
};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

macro_rules! note_select {
    () => {
        "SELECT n.id, n.band_id, n.author_id, u.username AS author_username,
                u.avatar_url AS author_avatar_url, n.content, n.color, n.is_pinned, n.due_at,
                n.created_at, n.updated_at,
                (SELECT x.username FROM users x WHERE x.id = n.updated_by) AS updated_by_username
         FROM band_notes n
         LEFT JOIN users u ON u.id = n.author_id"
    };
}

#[async_trait::async_trait]
pub trait BandNoteRepository: Send + Sync {
    /// Every reminder of the band: pinned first, then newest.
    async fn list(&self, band_id: Uuid) -> Result<Vec<BandNoteRow>, ApiError>;
    async fn find(&self, id: Uuid, band_id: Uuid) -> Result<Option<BandNoteRow>, ApiError>;
    /// Creates a reminder unless the band already has `max` (checked under
    /// a lock on the band, so concurrent creates can't overshoot). Returns
    /// `None` when full.
    async fn create(
        &self,
        band_id: Uuid,
        author_id: Uuid,
        payload: &CreateBandNotePayload,
        max: i64,
    ) -> Result<Option<Uuid>, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        band_id: Uuid,
        payload: &UpdateBandNotePayload,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;
    async fn delete(&self, id: Uuid, band_id: Uuid) -> Result<(), ApiError>;
}

pub struct BandNoteRepositoryImpl {
    pub db: PgPool,
}

impl BandNoteRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl BandNoteRepository for BandNoteRepositoryImpl {
    async fn list(&self, band_id: Uuid) -> Result<Vec<BandNoteRow>, ApiError> {
        let rows = sqlx::query_as::<_, BandNoteRow>(concat!(
            note_select!(),
            " WHERE n.band_id = $1 ORDER BY n.is_pinned DESC, n.created_at DESC, n.id"
        ))
        .bind(band_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows)
    }

    async fn find(&self, id: Uuid, band_id: Uuid) -> Result<Option<BandNoteRow>, ApiError> {
        let row = sqlx::query_as::<_, BandNoteRow>(concat!(
            note_select!(),
            " WHERE n.id = $1 AND n.band_id = $2"
        ))
        .bind(id)
        .bind(band_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row)
    }

    async fn create(
        &self,
        band_id: Uuid,
        author_id: Uuid,
        payload: &CreateBandNotePayload,
        max: i64,
    ) -> Result<Option<Uuid>, ApiError> {
        let mut tx = self.db.begin().await?;
        sqlx::query("SELECT id FROM bands WHERE id = $1 FOR UPDATE")
            .bind(band_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM band_notes WHERE band_id = $1")
            .bind(band_id)
            .fetch_one(&mut *tx)
            .await?;
        if count >= max {
            return Ok(None);
        }
        let id = Uuid::new_v4();
        let now = Utc::now().naive_utc();
        sqlx::query(
            "INSERT INTO band_notes (id, band_id, author_id, content, color, is_pinned, due_at, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
        )
        .bind(id)
        .bind(band_id)
        .bind(author_id)
        .bind(payload.content.trim())
        .bind(payload.color.unwrap_or(BandNoteColor::Default))
        .bind(payload.is_pinned.unwrap_or(false))
        .bind(payload.due_at)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    async fn update(
        &self,
        id: Uuid,
        band_id: Uuid,
        payload: &UpdateBandNotePayload,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        if payload.content.is_none()
            && payload.color.is_none()
            && payload.is_pinned.is_none()
            && payload.due_at.is_none()
        {
            return Err(ApiError::NotModified);
        }
        let result = sqlx::query(
            "UPDATE band_notes SET
                content = COALESCE($3, content),
                color = COALESCE($4, color),
                is_pinned = COALESCE($5, is_pinned),
                due_at = CASE WHEN $6 THEN $7 ELSE due_at END,
                updated_at = $8,
                updated_by = $9
             WHERE id = $1 AND band_id = $2",
        )
        .bind(id)
        .bind(band_id)
        .bind(payload.content.as_deref().map(str::trim))
        .bind(payload.color)
        .bind(payload.is_pinned)
        .bind(payload.due_at.is_some())
        .bind(payload.due_at.flatten())
        .bind(Utc::now().naive_utc())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn delete(&self, id: Uuid, band_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM band_notes WHERE id = $1 AND band_id = $2")
            .bind(id)
            .bind(band_id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }
}
