use crate::{
    errors::api_error::ApiError,
    models::moderation::{
        FlagListQuery, ModerationFlag, ModerationFlagRow, ModerationStatus, ModerationSummary,
        ModerationTarget, NewFlag,
    },
};
use chrono::NaiveDateTime;
use sqlx::PgPool;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Columns of a [`ModerationFlagRow`] from `moderation_flags f`.
macro_rules! flag_columns {
    () => {
        "f.id, f.target_type, f.user_id, u.username, u.avatar_url AS user_avatar_url,
         u.role AS user_role,
         (u.banned_at IS NOT NULL AND (u.banned_until IS NULL OR u.banned_until > (NOW() AT TIME ZONE 'utc'))) AS user_is_banned,
         f.band_id, b.name AS band_name, b.logo_url AS band_logo_url,
         f.value, f.reasons, f.score, f.details, f.source,
         (SELECT x.username FROM users x WHERE x.id = f.reported_by) AS reported_by_username,
         f.report_note, f.status, f.resolution, f.resolution_note,
         (SELECT x.username FROM users x WHERE x.id = f.resolved_by) AS resolved_by_username,
         f.resolved_at, f.created_at
         FROM moderation_flags f
         JOIN users u ON u.id = f.user_id
         LEFT JOIN bands b ON b.id = f.band_id"
    };
}

#[async_trait::async_trait]
pub trait ModerationRepository: Send + Sync {
    /// Stores an automatic flag unless the same value is already flagged
    /// and open. Returns whether a new flag was created.
    async fn create_automatic(&self, flag: &NewFlag) -> Result<bool, ApiError>;
    /// Stores a user report. Fails with `ALREADY_EXISTS` when the reporter
    /// already has an open report on this target.
    async fn create_report(&self, flag: &NewFlag) -> Result<Uuid, ApiError>;
    async fn list(
        &self,
        query: &FlagListQuery,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<ModerationFlag>, i64), ApiError>;
    async fn find(&self, id: Uuid) -> Result<Option<ModerationFlag>, ApiError>;
    async fn summary(&self) -> Result<ModerationSummary, ApiError>;
    /// Closes the flag and every other open flag on the same target and
    /// value. Returns how many flags were closed.
    async fn resolve(
        &self,
        flag: &ModerationFlag,
        status: ModerationStatus,
        resolution: &str,
        note: Option<&str>,
        resolver: Uuid,
    ) -> Result<u64, ApiError>;
    async fn count_open_for_user(&self, user_id: Uuid) -> Result<i64, ApiError>;
    /// Reports filed by `reporter` since `since` (rate limiting).
    async fn count_reports_since(
        &self,
        reporter: Uuid,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError>;
    async fn has_open_report(&self, reporter: Uuid, user_id: Uuid) -> Result<bool, ApiError>;
}

pub struct ModerationRepositoryImpl {
    pub db: PgPool,
}

impl ModerationRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    async fn insert(
        &self,
        flag: &NewFlag,
        ignore_duplicates: bool,
    ) -> Result<Option<Uuid>, ApiError> {
        let id = Uuid::now_v7();
        // `ON CONFLICT DO NOTHING` covers the partial unique indexes that
        // keep one open automatic flag per value.
        let query = if ignore_duplicates {
            "INSERT INTO moderation_flags (id, target_type, user_id, band_id, value, reasons, score,
                                           details, source, reported_by, report_note, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'open', $12)
             ON CONFLICT DO NOTHING
             RETURNING id"
        } else {
            "INSERT INTO moderation_flags (id, target_type, user_id, band_id, value, reasons, score,
                                           details, source, reported_by, report_note, status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'open', $12)
             RETURNING id"
        };
        let value: String = flag.value.chars().take(500).collect();
        let note: Option<String> = flag
            .report_note
            .as_ref()
            .map(|n| n.chars().take(500).collect());
        let inserted: Option<Uuid> = sqlx::query_scalar(query)
            .bind(id)
            .bind(flag.target_type)
            .bind(flag.user_id)
            .bind(flag.band_id)
            .bind(value)
            .bind(&flag.reasons)
            .bind(flag.score)
            .bind(&flag.details)
            .bind(flag.source)
            .bind(flag.reported_by)
            .bind(note)
            .bind(chrono::Utc::now().naive_utc())
            .fetch_optional(&self.db)
            .await?;
        Ok(inserted)
    }
}

#[async_trait::async_trait]
impl ModerationRepository for ModerationRepositoryImpl {
    async fn create_automatic(&self, flag: &NewFlag) -> Result<bool, ApiError> {
        Ok(self.insert(flag, true).await?.is_some())
    }

    async fn create_report(&self, flag: &NewFlag) -> Result<Uuid, ApiError> {
        match self.insert(flag, false).await {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(ApiError::AlreadyExists),
            Err(ApiError::DatabaseError(e)) if matches!(&e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505")) => {
                Err(ApiError::AlreadyExists)
            }
            Err(e) => Err(e),
        }
    }

    async fn list(
        &self,
        query: &FlagListQuery,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<ModerationFlag>, i64), ApiError> {
        let status: Option<ModerationStatus> = match query.status.as_deref().unwrap_or("open") {
            "all" => None,
            "dismissed" => Some(ModerationStatus::Dismissed),
            "actioned" => Some(ModerationStatus::Actioned),
            _ => Some(ModerationStatus::Open),
        };
        let offset = (page - 1) * per_page;

        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM moderation_flags f
             WHERE ($1::moderation_status IS NULL OR f.status = $1)
               AND ($2::moderation_target IS NULL OR f.target_type = $2)
               AND ($3::uuid IS NULL OR f.user_id = $3)",
        )
        .bind(status)
        .bind(query.target_type)
        .bind(query.user_id)
        .fetch_one(&self.db)
        .await?;

        let rows = sqlx::query_as::<_, ModerationFlagRow>(concat!(
            "SELECT ",
            flag_columns!(),
            " WHERE ($1::moderation_status IS NULL OR f.status = $1)
                AND ($2::moderation_target IS NULL OR f.target_type = $2)
                AND ($3::uuid IS NULL OR f.user_id = $3)
              ORDER BY f.created_at DESC
              LIMIT $4 OFFSET $5"
        ))
        .bind(status)
        .bind(query.target_type)
        .bind(query.user_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&self.db)
        .await?;

        Ok((rows.into_iter().map(ModerationFlag::from).collect(), total))
    }

    async fn find(&self, id: Uuid) -> Result<Option<ModerationFlag>, ApiError> {
        let row = sqlx::query_as::<_, ModerationFlagRow>(concat!(
            "SELECT ",
            flag_columns!(),
            " WHERE f.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(ModerationFlag::from))
    }

    async fn summary(&self) -> Result<ModerationSummary, ApiError> {
        let rows: Vec<(ModerationTarget, i64)> = sqlx::query_as(
            "SELECT target_type, COUNT(*) FROM moderation_flags WHERE status = 'open' GROUP BY target_type",
        )
        .fetch_all(&self.db)
        .await?;
        let mut by_type: BTreeMap<String, i64> = [
            ModerationTarget::Avatar,
            ModerationTarget::Username,
            ModerationTarget::BandLogo,
            ModerationTarget::Profile,
        ]
        .iter()
        .map(|t| (t.key().to_string(), 0))
        .collect();
        let mut open_total = 0;
        for (target, count) in rows {
            by_type.insert(target.key().to_string(), count);
            open_total += count;
        }
        Ok(ModerationSummary {
            open_total,
            by_type,
        })
    }

    async fn resolve(
        &self,
        flag: &ModerationFlag,
        status: ModerationStatus,
        resolution: &str,
        note: Option<&str>,
        resolver: Uuid,
    ) -> Result<u64, ApiError> {
        let now = chrono::Utc::now().naive_utc();
        let result = sqlx::query(
            "UPDATE moderation_flags
             SET status = $1, resolution = $2, resolution_note = $3, resolved_by = $4, resolved_at = $5
             WHERE status = 'open'
               AND (id = $6 OR (target_type = $7 AND user_id = $8
                                AND band_id IS NOT DISTINCT FROM $9 AND value = $10))",
        )
        .bind(status)
        .bind(resolution)
        .bind(note)
        .bind(resolver)
        .bind(now)
        .bind(flag.id)
        .bind(flag.target_type)
        .bind(flag.user.id)
        .bind(flag.band.as_ref().map(|b| b.id))
        .bind(&flag.value)
        .execute(&self.db)
        .await?;
        Ok(result.rows_affected())
    }

    async fn count_open_for_user(&self, user_id: Uuid) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM moderation_flags WHERE user_id = $1 AND status = 'open'",
        )
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }

    async fn count_reports_since(
        &self,
        reporter: Uuid,
        since: NaiveDateTime,
    ) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM moderation_flags
             WHERE reported_by = $1 AND source = 'report' AND created_at >= $2",
        )
        .bind(reporter)
        .bind(since)
        .fetch_one(&self.db)
        .await?)
    }

    async fn has_open_report(&self, reporter: Uuid, user_id: Uuid) -> Result<bool, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM moderation_flags
                            WHERE reported_by = $1 AND user_id = $2 AND source = 'report' AND status = 'open')",
        )
        .bind(reporter)
        .bind(user_id)
        .fetch_one(&self.db)
        .await?)
    }
}
