use crate::{
    errors::api_error::ApiError,
    models::{
        band::BandRole,
        gig::GigStatus,
        tour::{
            CreateTourPayload, GigSummary, Tour, TourGigSetlist, TourStatusFilter,
            UpdateTourPayload,
        },
    },
};
use chrono::{NaiveDateTime, Utc};
use sqlx::PgPool;
use tracing::error;
use uuid::Uuid;

/// Columns selected for a [`Tour`] from `tours t`.
macro_rules! tour_columns {
    () => {
        "t.id, t.user_id, t.band_id,
         (SELECT b.name FROM bands b WHERE b.id = t.band_id) AS band_name,
         t.name, t.description, t.start_date, t.end_date,
         (SELECT COUNT(*) FROM gigs g WHERE g.tour_id = t.id AND g.deleted_at IS NULL) AS gig_count,
         (SELECT MIN(g.scheduled_at) FROM gigs g
            WHERE g.tour_id = t.id AND g.deleted_at IS NULL AND g.status = 'confirmed'
              AND g.scheduled_at >= (NOW() AT TIME ZONE 'utc')) AS next_gig_at,
         t.created_at, t.updated_at,
         (SELECT u.username FROM users u WHERE u.id = t.updated_by) AS updated_by_username,
         (SELECT u.username FROM users u WHERE u.id = t.user_id) AS owner_username"
    };
}

fn filter_key(filter: TourStatusFilter) -> &'static str {
    match filter {
        TourStatusFilter::Upcoming => "upcoming",
        TourStatusFilter::Past => "past",
        TourStatusFilter::All => "all",
    }
}

#[async_trait::async_trait]
pub trait TourRepository: Send + Sync {
    /// The caller's live personal tours.
    async fn find_all(
        &self,
        user_id: Uuid,
        filter: TourStatusFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Tour>, i64), ApiError>;
    /// A band's live tours. Callers check membership first.
    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        filter: TourStatusFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Tour>, i64), ApiError>;
    /// A live tour the caller may view (their personal tour, or a tour of
    /// a band they belong to).
    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Tour>, ApiError>;
    /// Any live tour (no access filter).
    async fn find_any(&self, id: Uuid) -> Result<Option<Tour>, ApiError>;
    async fn create(&self, payload: &CreateTourPayload, user_id: Uuid) -> Result<Tour, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateTourPayload,
        actor_id: Uuid,
    ) -> Result<(), ApiError>;
    /// Moves a live tour to the trash (its gigs stay, without a tour).
    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError>;
    /// Personal owner, or a band member allowed to manage setlists.
    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError>;
    /// The live gigs of a tour, soonest first, each with its live setlist.
    async fn gigs(&self, tour_id: Uuid) -> Result<Vec<GigSummary>, ApiError>;
}

pub struct TourRepositoryImpl {
    pub db: PgPool,
}

impl TourRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct TourAccessRow {
    owner_id: Uuid,
    band_id: Option<Uuid>,
    band_role: Option<BandRole>,
    role_permission_allowed: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct GigSummaryRow {
    id: Uuid,
    venue: String,
    location: Option<String>,
    scheduled_at: NaiveDateTime,
    status: GigStatus,
    setlist_id: Option<Uuid>,
    setlist_title: Option<String>,
    setlist_is_repertoire: Option<bool>,
    setlist_song_count: Option<i64>,
    setlist_total_duration: Option<i32>,
}

#[async_trait::async_trait]
impl TourRepository for TourRepositoryImpl {
    async fn find_all(
        &self,
        user_id: Uuid,
        filter: TourStatusFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Tour>, i64), ApiError> {
        let offset = (page - 1) * size;
        let key = filter_key(filter);

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tours t
             WHERE t.user_id = $1 AND t.band_id IS NULL AND t.deleted_at IS NULL
               AND ($2 = 'all' OR ($2 = 'upcoming' AND t.end_date >= CURRENT_DATE)
                    OR ($2 = 'past' AND t.end_date < CURRENT_DATE))",
        )
        .bind(user_id)
        .bind(key)
        .fetch_one(&self.db);

        let tours = sqlx::query_as::<_, Tour>(concat!(
            "SELECT ",
            tour_columns!(),
            " FROM tours t
             WHERE t.user_id = $1 AND t.band_id IS NULL AND t.deleted_at IS NULL
               AND ($2 = 'all' OR ($2 = 'upcoming' AND t.end_date >= CURRENT_DATE)
                    OR ($2 = 'past' AND t.end_date < CURRENT_DATE))
             ORDER BY CASE WHEN $2 = 'upcoming' THEN t.start_date END ASC,
                      CASE WHEN $2 <> 'upcoming' THEN t.start_date END DESC,
                      LOWER(t.name) ASC, t.id ASC
             LIMIT $3 OFFSET $4"
        ))
        .bind(user_id)
        .bind(key)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, tours) = tokio::try_join!(count, tours)?;
        Ok((tours, count))
    }

    async fn find_all_for_band(
        &self,
        band_id: Uuid,
        filter: TourStatusFilter,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Tour>, i64), ApiError> {
        let offset = (page - 1) * size;
        let key = filter_key(filter);

        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tours t
             WHERE t.band_id = $1 AND t.deleted_at IS NULL
               AND ($2 = 'all' OR ($2 = 'upcoming' AND t.end_date >= CURRENT_DATE)
                    OR ($2 = 'past' AND t.end_date < CURRENT_DATE))",
        )
        .bind(band_id)
        .bind(key)
        .fetch_one(&self.db);

        let tours = sqlx::query_as::<_, Tour>(concat!(
            "SELECT ",
            tour_columns!(),
            " FROM tours t
             WHERE t.band_id = $1 AND t.deleted_at IS NULL
               AND ($2 = 'all' OR ($2 = 'upcoming' AND t.end_date >= CURRENT_DATE)
                    OR ($2 = 'past' AND t.end_date < CURRENT_DATE))
             ORDER BY CASE WHEN $2 = 'upcoming' THEN t.start_date END ASC,
                      CASE WHEN $2 <> 'upcoming' THEN t.start_date END DESC,
                      LOWER(t.name) ASC, t.id ASC
             LIMIT $3 OFFSET $4"
        ))
        .bind(band_id)
        .bind(key)
        .bind(size)
        .bind(offset)
        .fetch_all(&self.db);

        let (count, tours) = tokio::try_join!(count, tours)?;
        Ok((tours, count))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> Result<Option<Tour>, ApiError> {
        let tour = sqlx::query_as::<_, Tour>(concat!(
            "SELECT ",
            tour_columns!(),
            " FROM tours t
             LEFT JOIN band_members bm ON bm.band_id = t.band_id AND bm.user_id = $2
             WHERE t.id = $1 AND t.deleted_at IS NULL
               AND ((t.band_id IS NULL AND t.user_id = $2) OR bm.user_id IS NOT NULL)"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?;
        Ok(tour)
    }

    async fn find_any(&self, id: Uuid) -> Result<Option<Tour>, ApiError> {
        let tour = sqlx::query_as::<_, Tour>(concat!(
            "SELECT ",
            tour_columns!(),
            " FROM tours t WHERE t.id = $1 AND t.deleted_at IS NULL"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(tour)
    }

    async fn create(&self, payload: &CreateTourPayload, user_id: Uuid) -> Result<Tour, ApiError> {
        let id = Uuid::new_v4();
        let now = Utc::now().naive_utc();
        let description = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());

        sqlx::query(
            "INSERT INTO tours (id, user_id, band_id, name, description, start_date, end_date, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
        )
        .bind(id)
        .bind(user_id)
        .bind(payload.band_id)
        .bind(payload.name.trim())
        .bind(description)
        .bind(payload.start_date)
        .bind(payload.end_date)
        .bind(now)
        .execute(&self.db)
        .await?;

        self.find_any(id).await?.ok_or(ApiError::NotFound)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: &UpdateTourPayload,
        actor_id: Uuid,
    ) -> Result<(), ApiError> {
        let description = payload.description.as_ref().map(|d| {
            d.as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
        });
        if payload.name.is_none()
            && description.is_none()
            && payload.start_date.is_none()
            && payload.end_date.is_none()
        {
            return Err(ApiError::NotModified);
        }

        // `$3` says whether the description is part of the update (it may
        // be cleared to NULL).
        let result = sqlx::query(
            "UPDATE tours SET
                name = COALESCE($2, name),
                description = CASE WHEN $3 THEN $4 ELSE description END,
                start_date = COALESCE($5, start_date),
                end_date = COALESCE($6, end_date),
                updated_at = $7,
                updated_by = $8
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(payload.name.as_deref().map(str::trim))
        .bind(description.is_some())
        .bind(description.flatten())
        .bind(payload.start_date)
        .bind(payload.end_date)
        .bind(Utc::now().naive_utc())
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn trash(&self, id: Uuid, actor_id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query(
            "UPDATE tours SET deleted_at = $2, deleted_by = $3, trash_batch = $4
             WHERE id = $1 AND deleted_at IS NULL",
        )
        .bind(id)
        .bind(Utc::now().naive_utc())
        .bind(actor_id)
        .bind(Uuid::new_v4())
        .execute(&self.db)
        .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn can_manage(&self, id: Uuid, user_id: Uuid) -> Result<(), ApiError> {
        let row = sqlx::query_as::<_, TourAccessRow>(
            r#"
            SELECT
                t.user_id AS owner_id,
                t.band_id,
                bm.role AS band_role,
                brp.allowed AS role_permission_allowed
            FROM tours t
            LEFT JOIN band_members bm ON bm.band_id = t.band_id AND bm.user_id = $2
            LEFT JOIN band_role_permissions brp
                ON brp.band_id = t.band_id
                AND brp.role = bm.role
                AND brp.permission = 'manage_setlists'
            WHERE t.id = $1 AND t.deleted_at IS NULL
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await?
        .ok_or(ApiError::NotFound)?;

        let allowed = match row.band_id {
            None => row.owner_id == user_id,
            Some(_) => match row.band_role {
                Some(role) if role.satisfies(BandRole::Admin) => true,
                Some(_) => row.role_permission_allowed.unwrap_or(false),
                None => false,
            },
        };

        match (allowed, row.band_id, row.band_role) {
            (true, _, _) => Ok(()),
            (false, None, _) | (false, Some(_), None) => Err(ApiError::NotFound),
            (false, Some(_), Some(_)) => {
                error!(%id, %user_id, "User is not allowed to manage this tour.");
                Err(ApiError::Forbidden)
            }
        }
    }

    async fn gigs(&self, tour_id: Uuid) -> Result<Vec<GigSummary>, ApiError> {
        let rows = sqlx::query_as::<_, GigSummaryRow>(
            "SELECT g.id, g.venue, g.location, g.scheduled_at, g.status,
                    s.id AS setlist_id, s.title AS setlist_title, s.is_repertoire AS setlist_is_repertoire,
                    (SELECT COUNT(*) FROM setlist_songs ss
                        INNER JOIN songs so ON so.id = ss.song_id
                        WHERE ss.setlist_id = s.id AND so.deleted_at IS NULL
                          AND ((s.band_id IS NULL AND so.band_id IS NULL AND so.user_id = s.user_id)
                               OR so.band_id = s.band_id)) AS setlist_song_count,
                    CASE WHEN s.id IS NULL THEN NULL ELSE setlist_total_duration(s.id) END AS setlist_total_duration
             FROM gigs g
             LEFT JOIN setlists s ON s.id = g.setlist_id AND s.deleted_at IS NULL
             WHERE g.tour_id = $1 AND g.deleted_at IS NULL
             ORDER BY g.scheduled_at ASC, g.id ASC",
        )
        .bind(tour_id)
        .fetch_all(&self.db)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| GigSummary {
                id: r.id,
                venue: r.venue,
                location: r.location,
                scheduled_at: r.scheduled_at,
                status: r.status,
                setlist: match (r.setlist_id, r.setlist_title) {
                    (Some(id), Some(title)) => Some(TourGigSetlist {
                        id,
                        title,
                        is_repertoire: r.setlist_is_repertoire.unwrap_or(false),
                        song_count: r.setlist_song_count.unwrap_or(0),
                        total_duration: r.setlist_total_duration.unwrap_or(0),
                    }),
                    _ => None,
                },
            })
            .collect())
    }
}
