use crate::{
    errors::api_error::ApiError,
    models::pin::{MAX_PINS, PinItemType, PinRef, PinnedItem},
};
use chrono::Utc;
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;

/// Resolves pins of `p` (`user_pins p`) to live items the pin's owner can
/// still access: their personal content, or content of a band they are in.
macro_rules! resolved_pins {
    () => {
        "SELECT p.item_type, p.item_id, p.position, x.title, x.subtitle, x.band_id, x.is_repertoire
         FROM user_pins p
         JOIN LATERAL (
            SELECT s.title, a.name AS subtitle, s.band_id, FALSE AS is_repertoire FROM songs s
            INNER JOIN artists a ON a.id = s.artist_id
            WHERE p.item_type = 'song' AND s.id = p.item_id AND s.deleted_at IS NULL
              AND ((s.band_id IS NULL AND s.user_id = p.user_id)
                   OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = s.band_id AND m.user_id = p.user_id))
            UNION ALL
            SELECT st.title, (SELECT b.name FROM bands b WHERE b.id = st.band_id), st.band_id, st.is_repertoire FROM setlists st
            WHERE p.item_type = 'setlist' AND st.id = p.item_id AND st.deleted_at IS NULL
              AND ((st.band_id IS NULL AND st.user_id = p.user_id)
                   OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = st.band_id AND m.user_id = p.user_id))
            UNION ALL
            SELECT b.name, NULL, b.id, FALSE FROM bands b
            WHERE p.item_type = 'band' AND b.id = p.item_id
              AND EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = b.id AND m.user_id = p.user_id)
            UNION ALL
            SELECT t.name, (SELECT b.name FROM bands b WHERE b.id = t.band_id), t.band_id, FALSE FROM tours t
            WHERE p.item_type = 'tour' AND t.id = p.item_id AND t.deleted_at IS NULL
              AND ((t.band_id IS NULL AND t.user_id = p.user_id)
                   OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = t.band_id AND m.user_id = p.user_id))
            UNION ALL
            SELECT g.venue, to_char(g.scheduled_at, 'YYYY-MM-DD HH24:MI'), g.band_id, FALSE FROM gigs g
            WHERE p.item_type = 'gig' AND g.id = p.item_id AND g.deleted_at IS NULL
              AND ((g.band_id IS NULL AND g.user_id = p.user_id)
                   OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = g.band_id AND m.user_id = p.user_id))
         ) x ON TRUE"
    };
}

#[derive(sqlx::FromRow)]
struct PinRow {
    item_type: PinItemType,
    item_id: Uuid,
    position: i32,
    title: String,
    subtitle: Option<String>,
    band_id: Option<Uuid>,
    is_repertoire: bool,
}

fn href(item_type: PinItemType, id: Uuid) -> String {
    let section = match item_type {
        PinItemType::Setlist => "setlists",
        PinItemType::Band => "bands",
        PinItemType::Song => "songs",
        PinItemType::Tour => "tours",
        PinItemType::Gig => "gigs",
    };
    format!("/dashboard/{section}/{id}")
}

pub enum PinOutcome {
    Pinned,
    AlreadyPinned,
    Full,
}

#[async_trait::async_trait]
pub trait PinRepository: Send + Sync {
    /// The caller's pins that still resolve, in order.
    async fn list(&self, user_id: Uuid) -> Result<Vec<PinnedItem>, ApiError>;
    /// Whether `user_id` can currently see the item.
    async fn is_accessible(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<bool, ApiError>;
    /// Pins an item at the end (idempotent). Refuses past [`MAX_PINS`].
    async fn pin(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<PinOutcome, ApiError>;
    async fn unpin(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<(), ApiError>;
    /// Puts `items` first, in that order; other pins keep their relative
    /// order after them.
    async fn reorder(&self, user_id: Uuid, items: &[PinRef]) -> Result<(), ApiError>;
    /// Ids of the caller's pins of one kind (to flag `is_pinned`).
    async fn pinned_ids(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
    ) -> Result<HashSet<Uuid>, ApiError>;
}

pub struct PinRepositoryImpl {
    pub db: PgPool,
}

impl PinRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl PinRepository for PinRepositoryImpl {
    async fn list(&self, user_id: Uuid) -> Result<Vec<PinnedItem>, ApiError> {
        let rows = sqlx::query_as::<_, PinRow>(concat!(
            resolved_pins!(),
            " WHERE p.user_id = $1 ORDER BY p.position ASC, p.created_at ASC"
        ))
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| PinnedItem {
                item_type: r.item_type,
                item_id: r.item_id,
                position: r.position,
                title: r.title,
                subtitle: r.subtitle,
                band_id: r.band_id,
                is_repertoire: r.is_repertoire,
                href_hint: href(r.item_type, r.item_id),
            })
            .collect())
    }

    async fn is_accessible(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<bool, ApiError> {
        // Same resolution as the listing, on a virtual pin.
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM (SELECT $1::uuid AS user_id, $2::pin_item_type AS item_type, $3::uuid AS item_id) p
                JOIN LATERAL (
                    SELECT 1 AS ok FROM songs s
                    WHERE p.item_type = 'song' AND s.id = p.item_id AND s.deleted_at IS NULL
                      AND ((s.band_id IS NULL AND s.user_id = p.user_id)
                           OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = s.band_id AND m.user_id = p.user_id))
                    UNION ALL
                    SELECT 1 FROM setlists st
                    WHERE p.item_type = 'setlist' AND st.id = p.item_id AND st.deleted_at IS NULL
                      AND ((st.band_id IS NULL AND st.user_id = p.user_id)
                           OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = st.band_id AND m.user_id = p.user_id))
                    UNION ALL
                    SELECT 1 FROM bands b
                    WHERE p.item_type = 'band' AND b.id = p.item_id
                      AND EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = b.id AND m.user_id = p.user_id)
                    UNION ALL
                    SELECT 1 FROM tours t
                    WHERE p.item_type = 'tour' AND t.id = p.item_id AND t.deleted_at IS NULL
                      AND ((t.band_id IS NULL AND t.user_id = p.user_id)
                           OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = t.band_id AND m.user_id = p.user_id))
                    UNION ALL
                    SELECT 1 FROM gigs g
                    WHERE p.item_type = 'gig' AND g.id = p.item_id AND g.deleted_at IS NULL
                      AND ((g.band_id IS NULL AND g.user_id = p.user_id)
                           OR EXISTS (SELECT 1 FROM band_members m WHERE m.band_id = g.band_id AND m.user_id = p.user_id))
                ) x ON TRUE)",
        )
        .bind(user_id)
        .bind(item_type)
        .bind(item_id)
        .fetch_one(&self.db)
        .await?;
        Ok(found)
    }

    async fn pin(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<PinOutcome, ApiError> {
        let mut tx = self.db.begin().await?;
        // One pin change per account at a time, so the cap holds.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM user_pins WHERE user_id = $1 AND item_type = $2 AND item_id = $3)",
        )
        .bind(user_id)
        .bind(item_type)
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            return Ok(PinOutcome::AlreadyPinned);
        }

        // Stale pins (deleted or inaccessible items) don't count: they are
        // cleaned up here.
        let live: Vec<(PinItemType, Uuid)> = sqlx::query_as(concat!(
            "SELECT p.item_type, p.item_id FROM (",
            resolved_pins!(),
            " WHERE p.user_id = $1) p"
        ))
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let live_types: Vec<PinItemType> = live.iter().map(|(t, _)| *t).collect();
        let live_ids: Vec<Uuid> = live.iter().map(|(_, id)| *id).collect();
        sqlx::query(
            "DELETE FROM user_pins p WHERE p.user_id = $1
               AND NOT EXISTS (SELECT 1 FROM UNNEST($2::pin_item_type[], $3::uuid[]) AS l(t, i)
                               WHERE l.t = p.item_type AND l.i = p.item_id)",
        )
        .bind(user_id)
        .bind(&live_types)
        .bind(&live_ids)
        .execute(&mut *tx)
        .await?;

        if live.len() as i64 >= MAX_PINS {
            return Ok(PinOutcome::Full);
        }

        sqlx::query(
            "INSERT INTO user_pins (user_id, item_type, item_id, position, created_at)
             VALUES ($1, $2, $3,
                     COALESCE((SELECT MAX(position) FROM user_pins WHERE user_id = $1), 0) + 1, $4)",
        )
        .bind(user_id)
        .bind(item_type)
        .bind(item_id)
        .bind(Utc::now().naive_utc())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(PinOutcome::Pinned)
    }

    async fn unpin(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
        item_id: Uuid,
    ) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM user_pins WHERE user_id = $1 AND item_type = $2 AND item_id = $3")
            .bind(user_id)
            .bind(item_type)
            .bind(item_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    async fn reorder(&self, user_id: Uuid, items: &[PinRef]) -> Result<(), ApiError> {
        let mut tx = self.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        let current: Vec<(PinItemType, Uuid)> = sqlx::query_as(
            "SELECT item_type, item_id FROM user_pins WHERE user_id = $1 ORDER BY position, created_at",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut ordered: Vec<(PinItemType, Uuid)> = Vec::with_capacity(current.len());
        for item in items {
            let key = (item.item_type, item.item_id);
            if current.contains(&key) && !ordered.contains(&key) {
                ordered.push(key);
            }
        }
        for key in current {
            if !ordered.contains(&key) {
                ordered.push(key);
            }
        }

        let types: Vec<PinItemType> = ordered.iter().map(|(t, _)| *t).collect();
        let ids: Vec<Uuid> = ordered.iter().map(|(_, id)| *id).collect();
        let positions: Vec<i32> = (1..=ordered.len() as i32).collect();
        sqlx::query(
            "UPDATE user_pins p SET position = u.position
             FROM UNNEST($2::pin_item_type[], $3::uuid[], $4::int[]) AS u(t, i, position)
             WHERE p.user_id = $1 AND p.item_type = u.t AND p.item_id = u.i",
        )
        .bind(user_id)
        .bind(&types)
        .bind(&ids)
        .bind(&positions)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn pinned_ids(
        &self,
        user_id: Uuid,
        item_type: PinItemType,
    ) -> Result<HashSet<Uuid>, ApiError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT item_id FROM user_pins WHERE user_id = $1 AND item_type = $2",
        )
        .bind(user_id)
        .bind(item_type)
        .fetch_all(&self.db)
        .await?;
        Ok(ids.into_iter().collect())
    }
}
