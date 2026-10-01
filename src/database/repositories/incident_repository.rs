use crate::{
    errors::api_error::ApiError,
    models::incident::{
        CreateIncidentPayload, Incident, IncidentRow, IncidentStatus, IncidentUpdate,
        UpdateIncidentPayload,
    },
};
use chrono::{NaiveDateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;

macro_rules! incident_columns {
    () => {
        "id, kind, title, impact, status, components, scheduled_for, scheduled_until,
         started_at, resolved_at, created_at, updated_at"
    };
}

#[async_trait::async_trait]
pub trait IncidentRepository: Send + Sync {
    async fn create(
        &self,
        payload: &CreateIncidentPayload,
        status: IncidentStatus,
        author: Uuid,
    ) -> Result<Incident, ApiError>;
    async fn find(&self, id: Uuid) -> Result<Option<Incident>, ApiError>;
    async fn list(
        &self,
        active_only: bool,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Incident>, i64), ApiError>;
    /// Unresolved incidents, and the ones resolved since `since`.
    async fn public(
        &self,
        since: NaiveDateTime,
    ) -> Result<(Vec<Incident>, Vec<Incident>), ApiError>;
    async fn update(&self, id: Uuid, payload: &UpdateIncidentPayload) -> Result<(), ApiError>;
    /// Adds a timeline entry and moves the incident to its status.
    async fn post_update(
        &self,
        id: Uuid,
        status: IncidentStatus,
        body: &str,
        author: Uuid,
    ) -> Result<(), ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    /// Unresolved incidents (the console's overview).
    async fn count_active(&self) -> Result<i64, ApiError>;
}

pub struct IncidentRepositoryImpl {
    pub db: PgPool,
}

impl IncidentRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Attaches every row's timeline, newest update first.
    async fn with_updates(&self, rows: Vec<IncidentRow>) -> Result<Vec<Incident>, ApiError> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let updates = sqlx::query_as::<_, IncidentUpdate>(
            "SELECT id, incident_id, status, body, created_at FROM status_incident_updates
             WHERE incident_id = ANY($1) ORDER BY created_at DESC, id DESC",
        )
        .bind(&ids)
        .fetch_all(&self.db)
        .await?;
        let mut by_incident: HashMap<Uuid, Vec<IncidentUpdate>> = HashMap::new();
        for update in updates {
            by_incident
                .entry(update.incident_id)
                .or_default()
                .push(update);
        }
        Ok(rows
            .into_iter()
            .map(|incident| Incident {
                updates: by_incident.remove(&incident.id).unwrap_or_default(),
                incident,
            })
            .collect())
    }
}

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Moves incident `id` to `status` with the timestamps that go with it.
async fn apply_status(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    status: IncidentStatus,
    now: NaiveDateTime,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE status_incidents
         SET status = $2,
             -- A maintenance resolved before it ever began (cancelled)
             -- never started.
             started_at = CASE WHEN $2 = 'scheduled' THEN started_at
                               WHEN $2 = 'resolved' AND status = 'scheduled' THEN started_at
                               ELSE COALESCE(started_at, $3) END,
             resolved_at = CASE WHEN $2 = 'resolved' THEN COALESCE(resolved_at, $3) ELSE NULL END,
             updated_at = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_update(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    status: IncidentStatus,
    body: &str,
    author: Uuid,
    now: NaiveDateTime,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO status_incident_updates (id, incident_id, status, body, author_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(status)
    .bind(body.trim())
    .bind(author)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl IncidentRepository for IncidentRepositoryImpl {
    async fn create(
        &self,
        payload: &CreateIncidentPayload,
        status: IncidentStatus,
        author: Uuid,
    ) -> Result<Incident, ApiError> {
        let now = now();
        let id = Uuid::now_v7();
        let mut components = payload.components.clone();
        components.sort();
        components.dedup();
        let mut tx = self.db.begin().await?;
        sqlx::query(
            "INSERT INTO status_incidents (id, kind, title, impact, status, components,
                                           scheduled_for, scheduled_until, created_by,
                                           created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'scheduled', $5, $6, $7, $8, $9, $9)",
        )
        .bind(id)
        .bind(payload.kind)
        .bind(payload.title.trim())
        .bind(payload.impact)
        .bind(&components)
        .bind(payload.scheduled_for)
        .bind(payload.scheduled_until)
        .bind(author)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        apply_status(&mut tx, id, status, now).await?;
        insert_update(&mut tx, id, status, &payload.message, author, now).await?;
        tx.commit().await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn find(&self, id: Uuid) -> Result<Option<Incident>, ApiError> {
        let row = sqlx::query_as::<_, IncidentRow>(concat!(
            "SELECT ",
            incident_columns!(),
            " FROM status_incidents WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(match row {
            Some(row) => self.with_updates(vec![row]).await?.pop(),
            None => None,
        })
    }

    async fn list(
        &self,
        active_only: bool,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Incident>, i64), ApiError> {
        let total = sqlx::query_scalar(
            "SELECT COUNT(*) FROM status_incidents WHERE ($1 = FALSE OR status <> 'resolved')",
        )
        .bind(active_only)
        .fetch_one(&self.db);
        let rows = sqlx::query_as::<_, IncidentRow>(concat!(
            "SELECT ",
            incident_columns!(),
            " FROM status_incidents WHERE ($1 = FALSE OR status <> 'resolved')
              ORDER BY (status <> 'resolved') DESC, created_at DESC LIMIT $2 OFFSET $3"
        ))
        .bind(active_only)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db);
        let (total, rows) = tokio::try_join!(total, rows)?;
        Ok((self.with_updates(rows).await?, total))
    }

    async fn public(
        &self,
        since: NaiveDateTime,
    ) -> Result<(Vec<Incident>, Vec<Incident>), ApiError> {
        let rows = sqlx::query_as::<_, IncidentRow>(concat!(
            "SELECT ",
            incident_columns!(),
            " FROM status_incidents
              WHERE status <> 'resolved' OR resolved_at >= $1
              ORDER BY (status <> 'resolved') DESC, created_at DESC LIMIT 50"
        ))
        .bind(since)
        .fetch_all(&self.db)
        .await?;
        let (active, recent): (Vec<Incident>, Vec<Incident>) = self
            .with_updates(rows)
            .await?
            .into_iter()
            .partition(|i| i.incident.status != IncidentStatus::Resolved);
        Ok((active, recent))
    }

    async fn update(&self, id: Uuid, payload: &UpdateIncidentPayload) -> Result<(), ApiError> {
        let components = payload.components.clone().map(|mut c| {
            c.sort();
            c.dedup();
            c
        });
        sqlx::query(
            "UPDATE status_incidents
             SET title = COALESCE($2, title),
                 impact = COALESCE($3, impact),
                 components = COALESCE($4, components),
                 scheduled_for = CASE WHEN $5 THEN $6 ELSE scheduled_for END,
                 scheduled_until = CASE WHEN $7 THEN $8 ELSE scheduled_until END,
                 updated_at = $9
             WHERE id = $1",
        )
        .bind(id)
        .bind(payload.title.as_deref().map(str::trim))
        .bind(payload.impact)
        .bind(components)
        .bind(payload.scheduled_for.is_some())
        .bind(payload.scheduled_for.flatten())
        .bind(payload.scheduled_until.is_some())
        .bind(payload.scheduled_until.flatten())
        .bind(now())
        .execute(&self.db)
        .await
        .map_err(|e| match &e {
            // The window check lost a race with another edit of the other
            // end of the window: the database's constraint caught it.
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23514") => {
                ApiError::BadRequest("The window must end after it starts.".into())
            }
            _ => ApiError::from(e),
        })?;
        Ok(())
    }

    async fn post_update(
        &self,
        id: Uuid,
        status: IncidentStatus,
        body: &str,
        author: Uuid,
    ) -> Result<(), ApiError> {
        let now = now();
        let mut tx = self.db.begin().await?;
        apply_status(&mut tx, id, status, now).await?;
        insert_update(&mut tx, id, status, body, author, now).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM status_incidents WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn count_active(&self) -> Result<i64, ApiError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM status_incidents WHERE status <> 'resolved'")
                .fetch_one(&self.db)
                .await?,
        )
    }
}
