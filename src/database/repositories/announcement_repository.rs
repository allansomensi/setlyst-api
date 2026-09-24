//! Announcements, their audience and their read receipts.
//!
//! The audience is evaluated in SQL (see `audience_filter!`), so counting,
//! listing and fanning out always agree on who is targeted.

use crate::{
    email::{EmailTemplate, OutgoingEmail, outbox::enqueue_many_in},
    errors::api_error::ApiError,
    models::announcement::{
        Announcement, AnnouncementDraft, AnnouncementReceipt, AnnouncementStats, AnnouncementStatus,
    },
};
use chrono::{NaiveDateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Columns of an [`Announcement`] from `announcements a`.
macro_rules! announcement_columns {
    () => {
        "a.id, a.title, a.body, a.level, a.show_modal, a.show_banner, a.send_notification, a.send_email,
         a.dismissible, a.requires_acknowledgement, a.cta_label, a.cta_url, a.audience_roles,
         a.audience_plans, a.audience_locales, a.starts_at, a.ends_at, a.published_at, a.archived_at,
         a.delivered_at,
         (SELECT x.username FROM users x WHERE x.id = a.created_by) AS created_by_username,
         (SELECT x.username FROM users x WHERE x.id = a.updated_by) AS updated_by_username,
         a.created_at, a.updated_at"
    };
}

/// Accounts matched by an audience: active, not suspended, and within
/// every restricted dimension (roles, plans, locales). Expects `users u`,
/// `user_preferences up` and `subscriptions s` (both left-joined). The
/// arguments are the SQL expressions holding each audience list.
macro_rules! audience_filter {
    ($roles:literal, $plans:literal, $locales:literal) => {
        concat!(
            "u.status = 'active'
             AND NOT (u.banned_at IS NOT NULL AND (u.banned_until IS NULL OR u.banned_until > (NOW() AT TIME ZONE 'utc')))
             AND (", $roles, " IS NULL OR u.role::text = ANY(", $roles, "))
             AND (", $locales, " IS NULL OR COALESCE(up.language, 'en') = ANY(", $locales, "))
             AND (", $plans, " IS NULL
                  OR ('none' = ANY(", $plans, ") AND NOT (s.user_id IS NOT NULL
                        AND s.status IN ('trialing', 'active', 'past_due')
                        AND (s.current_period_end IS NULL OR s.current_period_end > (NOW() AT TIME ZONE 'utc'))))
                  OR ('trial' = ANY(", $plans, ") AND s.status = 'trialing'
                        AND (s.current_period_end IS NULL OR s.current_period_end > (NOW() AT TIME ZONE 'utc')))
                  OR (s.status IN ('trialing', 'active', 'past_due')
                        AND (s.current_period_end IS NULL OR s.current_period_end > (NOW() AT TIME ZONE 'utc'))
                        AND s.plan_code = ANY(", $plans, ")))"
        )
    };
}

/// Matches the computed status (`$1`, `NULL` = any) at time `$2`, in SQL
/// so filtering and paging agree with `Announcement::compute_status`.
macro_rules! status_filter {
    () => {
        "($1::text IS NULL OR $1 = CASE
             WHEN a.archived_at IS NOT NULL THEN 'archived'
             WHEN a.published_at IS NULL THEN 'draft'
             WHEN a.starts_at IS NOT NULL AND a.starts_at > $2 THEN 'scheduled'
             WHEN a.ends_at IS NOT NULL AND a.ends_at <= $2 THEN 'ended'
             ELSE 'active' END)"
    };
}

/// The join the audience filter expects.
macro_rules! audience_from {
    () => {
        "users u
         LEFT JOIN user_preferences up ON up.user_id = u.id
         LEFT JOIN subscriptions s ON s.user_id = u.id"
    };
}

/// `true` when the caller (`$1`) is in announcement `a`'s audience.
macro_rules! caller_targeted {
    () => {
        concat!(
            "EXISTS (SELECT 1 FROM ",
            audience_from!(),
            " WHERE u.id = $1 AND ",
            audience_filter!("a.audience_roles", "a.audience_plans", "a.audience_locales"),
            ")"
        )
    };
}

/// A recipient of an announcement e-mail.
#[derive(Debug, Clone, FromRow)]
pub struct EmailRecipient {
    pub user_id: Uuid,
    pub email: String,
    pub language: String,
}

/// What a fan-out did.
#[derive(Debug, Clone, Default)]
pub struct FanOut {
    pub notifications: u64,
    /// E-mails queued (to verified addresses; zero unless the announcement
    /// is sent by e-mail).
    pub emails: u64,
}

#[derive(Debug, Clone, FromRow)]
struct AnnouncementWithReceipt {
    #[sqlx(flatten)]
    announcement: Announcement,
    seen_at: Option<NaiveDateTime>,
    dismissed_at: Option<NaiveDateTime>,
    acknowledged_at: Option<NaiveDateTime>,
}

#[async_trait::async_trait]
pub trait AnnouncementRepository: Send + Sync {
    async fn create(
        &self,
        draft: &AnnouncementDraft,
        actor_id: Uuid,
    ) -> Result<Announcement, ApiError>;
    async fn update(
        &self,
        id: Uuid,
        draft: &AnnouncementDraft,
        actor_id: Uuid,
    ) -> Result<Announcement, ApiError>;
    async fn find(&self, id: Uuid) -> Result<Option<Announcement>, ApiError>;
    async fn list(
        &self,
        status: Option<AnnouncementStatus>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Announcement>, i64), ApiError>;
    async fn stats(&self, announcement: &Announcement) -> Result<AnnouncementStats, ApiError>;
    /// Accounts an audience currently matches.
    async fn count_audience(
        &self,
        roles: Option<&[String]>,
        plans: Option<&[String]>,
        locales: Option<&[String]>,
    ) -> Result<i64, ApiError>;
    async fn delete(&self, id: Uuid) -> Result<(), ApiError>;
    async fn publish(&self, id: Uuid, actor_id: Uuid) -> Result<Announcement, ApiError>;
    async fn archive(&self, id: Uuid, actor_id: Uuid) -> Result<Announcement, ApiError>;

    /// Published announcements targeted at `user_id` whose window started
    /// (ended ones included, archived ones not), newest first.
    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<(Announcement, AnnouncementReceipt)>, i64), ApiError>;
    /// Active announcements targeted at `user_id`, oldest first.
    async fn active_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<(Announcement, AnnouncementReceipt)>, ApiError>;
    /// The announcement if it is visible to `user_id`.
    async fn find_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<(Announcement, AnnouncementReceipt)>, ApiError>;
    /// Sets the receipt timestamps given as `true` (keeping earlier ones).
    async fn mark_receipt(
        &self,
        id: Uuid,
        user_id: Uuid,
        seen: bool,
        dismissed: bool,
        acknowledged: bool,
    ) -> Result<AnnouncementReceipt, ApiError>;

    /// Published, started announcements not delivered yet.
    async fn due_for_delivery(&self) -> Result<Vec<Uuid>, ApiError>;
    /// Claims the announcement for delivery (once) and creates the in-app
    /// notifications in one statement, honouring each recipient's
    /// `announcements.in_app` preference. Returns `None` when it was not
    /// due or already delivered.
    async fn fan_out(&self, id: Uuid) -> Result<Option<(Announcement, FanOut)>, ApiError>;
}

pub struct AnnouncementRepositoryImpl {
    pub db: PgPool,
}

impl AnnouncementRepositoryImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

fn split(row: AnnouncementWithReceipt, now: NaiveDateTime) -> (Announcement, AnnouncementReceipt) {
    (
        row.announcement.with_status(now),
        AnnouncementReceipt {
            seen_at: row.seen_at,
            dismissed_at: row.dismissed_at,
            acknowledged_at: row.acknowledged_at,
        },
    )
}

#[async_trait::async_trait]
impl AnnouncementRepository for AnnouncementRepositoryImpl {
    async fn create(
        &self,
        draft: &AnnouncementDraft,
        actor_id: Uuid,
    ) -> Result<Announcement, ApiError> {
        let id = Uuid::now_v7();
        let timestamp = now();
        sqlx::query(
            "INSERT INTO announcements (id, title, body, level, show_modal, show_banner, send_notification,
                                        send_email, dismissible, requires_acknowledgement, cta_label, cta_url,
                                        audience_roles, audience_plans, audience_locales, starts_at, ends_at,
                                        created_by, updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $18, $19, $19)",
        )
        .bind(id)
        .bind(draft.title.trim())
        .bind(draft.body.trim())
        .bind(draft.level)
        .bind(draft.show_modal)
        .bind(draft.show_banner)
        .bind(draft.send_notification)
        .bind(draft.send_email)
        .bind(draft.dismissible)
        .bind(draft.requires_acknowledgement)
        .bind(draft.cta_label.as_deref().map(str::trim))
        .bind(draft.cta_url.as_deref().map(str::trim))
        .bind(&draft.audience_roles)
        .bind(&draft.audience_plans)
        .bind(&draft.audience_locales)
        .bind(draft.starts_at)
        .bind(draft.ends_at)
        .bind(actor_id)
        .bind(timestamp)
        .execute(&self.db)
        .await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn update(
        &self,
        id: Uuid,
        draft: &AnnouncementDraft,
        actor_id: Uuid,
    ) -> Result<Announcement, ApiError> {
        sqlx::query(
            "UPDATE announcements SET
                 title = $2, body = $3, level = $4, show_modal = $5, show_banner = $6,
                 send_notification = $7, send_email = $8, dismissible = $9,
                 requires_acknowledgement = $10, cta_label = $11, cta_url = $12,
                 audience_roles = $13, audience_plans = $14, audience_locales = $15,
                 starts_at = $16, ends_at = $17, updated_by = $18, updated_at = $19
             WHERE id = $1",
        )
        .bind(id)
        .bind(draft.title.trim())
        .bind(draft.body.trim())
        .bind(draft.level)
        .bind(draft.show_modal)
        .bind(draft.show_banner)
        .bind(draft.send_notification)
        .bind(draft.send_email)
        .bind(draft.dismissible)
        .bind(draft.requires_acknowledgement)
        .bind(draft.cta_label.as_deref().map(str::trim))
        .bind(draft.cta_url.as_deref().map(str::trim))
        .bind(&draft.audience_roles)
        .bind(&draft.audience_plans)
        .bind(&draft.audience_locales)
        .bind(draft.starts_at)
        .bind(draft.ends_at)
        .bind(actor_id)
        .bind(now())
        .execute(&self.db)
        .await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn find(&self, id: Uuid) -> Result<Option<Announcement>, ApiError> {
        let row = sqlx::query_as::<_, Announcement>(concat!(
            "SELECT ",
            announcement_columns!(),
            " FROM announcements a WHERE a.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|a| a.with_status(now())))
    }

    async fn list(
        &self,
        status: Option<AnnouncementStatus>,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<Announcement>, i64), ApiError> {
        let status_key = status.map(|s| match s {
            AnnouncementStatus::Draft => "draft",
            AnnouncementStatus::Scheduled => "scheduled",
            AnnouncementStatus::Active => "active",
            AnnouncementStatus::Ended => "ended",
            AnnouncementStatus::Archived => "archived",
        });
        let timestamp = now();
        let total: i64 = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM announcements a WHERE ",
            status_filter!()
        ))
        .bind(status_key)
        .bind(timestamp)
        .fetch_one(&self.db)
        .await?;
        let rows = sqlx::query_as::<_, Announcement>(concat!(
            "SELECT ",
            announcement_columns!(),
            " FROM announcements a WHERE ",
            status_filter!(),
            " ORDER BY a.created_at DESC LIMIT $3 OFFSET $4"
        ))
        .bind(status_key)
        .bind(timestamp)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db)
        .await?;
        Ok((
            rows.into_iter().map(|a| a.with_status(timestamp)).collect(),
            total,
        ))
    }

    async fn stats(&self, announcement: &Announcement) -> Result<AnnouncementStats, ApiError> {
        let targeted = self
            .count_audience(
                announcement.audience_roles.as_deref(),
                announcement.audience_plans.as_deref(),
                announcement.audience_locales.as_deref(),
            )
            .await?;
        let (seen, dismissed, acknowledged): (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*) FILTER (WHERE seen_at IS NOT NULL),
                    COUNT(*) FILTER (WHERE dismissed_at IS NOT NULL),
                    COUNT(*) FILTER (WHERE acknowledged_at IS NOT NULL)
             FROM announcement_receipts WHERE announcement_id = $1",
        )
        .bind(announcement.id)
        .fetch_one(&self.db)
        .await?;
        Ok(AnnouncementStats {
            targeted,
            seen,
            dismissed,
            acknowledged,
        })
    }

    async fn count_audience(
        &self,
        roles: Option<&[String]>,
        plans: Option<&[String]>,
        locales: Option<&[String]>,
    ) -> Result<i64, ApiError> {
        Ok(sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM ",
            audience_from!(),
            " WHERE ",
            audience_filter!("$1::text[]", "$2::text[]", "$3::text[]")
        ))
        .bind(roles)
        .bind(plans)
        .bind(locales)
        .fetch_one(&self.db)
        .await?)
    }

    async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let result = sqlx::query("DELETE FROM announcements WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
        Ok(())
    }

    async fn publish(&self, id: Uuid, actor_id: Uuid) -> Result<Announcement, ApiError> {
        let timestamp = now();
        sqlx::query(
            "UPDATE announcements SET published_at = COALESCE(published_at, $2), updated_by = $3, updated_at = $2
             WHERE id = $1",
        )
        .bind(id)
        .bind(timestamp)
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn archive(&self, id: Uuid, actor_id: Uuid) -> Result<Announcement, ApiError> {
        let timestamp = now();
        sqlx::query(
            "UPDATE announcements SET archived_at = COALESCE(archived_at, $2), updated_by = $3, updated_at = $2
             WHERE id = $1",
        )
        .bind(id)
        .bind(timestamp)
        .bind(actor_id)
        .execute(&self.db)
        .await?;
        self.find(id).await?.ok_or(ApiError::NotFound)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        page: i64,
        per_page: i64,
    ) -> Result<(Vec<(Announcement, AnnouncementReceipt)>, i64), ApiError> {
        let timestamp = now();
        let total: i64 = sqlx::query_scalar(concat!(
            "SELECT COUNT(*) FROM announcements a
             WHERE a.published_at IS NOT NULL AND a.archived_at IS NULL
               AND (a.starts_at IS NULL OR a.starts_at <= $2) AND ",
            caller_targeted!()
        ))
        .bind(user_id)
        .bind(timestamp)
        .fetch_one(&self.db)
        .await?;
        let rows = sqlx::query_as::<_, AnnouncementWithReceipt>(concat!(
            "SELECT ",
            announcement_columns!(),
            ", r.seen_at, r.dismissed_at, r.acknowledged_at
             FROM announcements a
             LEFT JOIN announcement_receipts r ON r.announcement_id = a.id AND r.user_id = $1
             WHERE a.published_at IS NOT NULL AND a.archived_at IS NULL
               AND (a.starts_at IS NULL OR a.starts_at <= $2) AND ",
            caller_targeted!(),
            " ORDER BY COALESCE(a.starts_at, a.published_at) DESC LIMIT $3 OFFSET $4"
        ))
        .bind(user_id)
        .bind(timestamp)
        .bind(per_page)
        .bind((page - 1) * per_page)
        .fetch_all(&self.db)
        .await?;
        Ok((
            rows.into_iter().map(|r| split(r, timestamp)).collect(),
            total,
        ))
    }

    async fn active_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<(Announcement, AnnouncementReceipt)>, ApiError> {
        let timestamp = now();
        let rows = sqlx::query_as::<_, AnnouncementWithReceipt>(concat!(
            "SELECT ",
            announcement_columns!(),
            ", r.seen_at, r.dismissed_at, r.acknowledged_at
             FROM announcements a
             LEFT JOIN announcement_receipts r ON r.announcement_id = a.id AND r.user_id = $1
             WHERE a.published_at IS NOT NULL AND a.archived_at IS NULL
               AND (a.starts_at IS NULL OR a.starts_at <= $2)
               AND (a.ends_at IS NULL OR a.ends_at > $2)
               AND (a.show_modal OR a.show_banner) AND ",
            caller_targeted!(),
            " ORDER BY COALESCE(a.starts_at, a.published_at) ASC LIMIT 50"
        ))
        .bind(user_id)
        .bind(timestamp)
        .fetch_all(&self.db)
        .await?;
        Ok(rows.into_iter().map(|r| split(r, timestamp)).collect())
    }

    async fn find_for_user(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<(Announcement, AnnouncementReceipt)>, ApiError> {
        let timestamp = now();
        let row = sqlx::query_as::<_, AnnouncementWithReceipt>(concat!(
            "SELECT ",
            announcement_columns!(),
            ", r.seen_at, r.dismissed_at, r.acknowledged_at
             FROM announcements a
             LEFT JOIN announcement_receipts r ON r.announcement_id = a.id AND r.user_id = $1
             WHERE a.id = $3 AND a.published_at IS NOT NULL AND a.archived_at IS NULL
               AND (a.starts_at IS NULL OR a.starts_at <= $2) AND ",
            caller_targeted!()
        ))
        .bind(user_id)
        .bind(timestamp)
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|r| split(r, timestamp)))
    }

    async fn mark_receipt(
        &self,
        id: Uuid,
        user_id: Uuid,
        seen: bool,
        dismissed: bool,
        acknowledged: bool,
    ) -> Result<AnnouncementReceipt, ApiError> {
        let timestamp = now();
        Ok(sqlx::query_as::<_, AnnouncementReceipt>(
            "INSERT INTO announcement_receipts (announcement_id, user_id, seen_at, dismissed_at, acknowledged_at)
             VALUES ($1, $2, $3,
                     CASE WHEN $4 THEN $3 END,
                     CASE WHEN $5 THEN $3 END)
             ON CONFLICT (announcement_id, user_id) DO UPDATE SET
                 seen_at = COALESCE(announcement_receipts.seen_at, $3),
                 dismissed_at = CASE WHEN $4 THEN COALESCE(announcement_receipts.dismissed_at, $3)
                                     ELSE announcement_receipts.dismissed_at END,
                 acknowledged_at = CASE WHEN $5 THEN COALESCE(announcement_receipts.acknowledged_at, $3)
                                        ELSE announcement_receipts.acknowledged_at END
             RETURNING seen_at, dismissed_at, acknowledged_at",
        )
        .bind(id)
        .bind(user_id)
        .bind(if seen || dismissed || acknowledged {
            Some(timestamp)
        } else {
            None
        })
        .bind(dismissed)
        .bind(acknowledged)
        .fetch_one(&self.db)
        .await?)
    }

    async fn due_for_delivery(&self) -> Result<Vec<Uuid>, ApiError> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM announcements
             WHERE published_at IS NOT NULL AND archived_at IS NULL AND delivered_at IS NULL
               AND (starts_at IS NULL OR starts_at <= $1)
             ORDER BY published_at
             LIMIT 20",
        )
        .bind(now())
        .fetch_all(&self.db)
        .await?)
    }

    async fn fan_out(&self, id: Uuid) -> Result<Option<(Announcement, FanOut)>, ApiError> {
        let timestamp = now();
        let mut tx = self.db.begin().await?;

        // Claim: only one worker (or the in-process trigger) ever gets
        // past this for a given announcement.
        let claimed: Option<Uuid> = sqlx::query_scalar(
            "UPDATE announcements SET delivered_at = $2
             WHERE id = $1 AND delivered_at IS NULL AND published_at IS NOT NULL
               AND archived_at IS NULL AND (starts_at IS NULL OR starts_at <= $2)
             RETURNING id",
        )
        .bind(id)
        .bind(timestamp)
        .fetch_optional(&mut *tx)
        .await?;
        if claimed.is_none() {
            return Ok(None);
        }

        let announcement = sqlx::query_as::<_, Announcement>(concat!(
            "SELECT ",
            announcement_columns!(),
            " FROM announcements a WHERE a.id = $1"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?
        .with_status(timestamp);

        let ended = announcement.ends_at.is_some_and(|end| end <= timestamp);
        let mut result = FanOut::default();

        if announcement.send_notification && !ended {
            result.notifications = sqlx::query(concat!(
                "INSERT INTO notifications (id, user_id, type, data, read_at, created_at)
                 SELECT gen_random_uuid(), u.id, 'announcement',
                        jsonb_build_object('announcement_id', a.id, 'title', a.title, 'level', a.level),
                        NULL, $2
                 FROM announcements a, ",
                audience_from!(),
                " WHERE a.id = $1 AND ",
                audience_filter!("a.audience_roles", "a.audience_plans", "a.audience_locales"),
                " AND COALESCE((up.communication->'categories'->'announcements'->>'in_app')::boolean, TRUE)"
            ))
            .bind(id)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }

        if announcement.send_email && !ended {
            // Critical announcements are service notices: every targeted
            // verified address gets them.
            let recipients = sqlx::query_as::<_, EmailRecipient>(concat!(
                "SELECT u.id AS user_id, u.email, COALESCE(up.language, 'en') AS language
                 FROM announcements a, ",
                audience_from!(),
                " WHERE a.id = $1 AND ",
                audience_filter!("a.audience_roles", "a.audience_plans", "a.audience_locales"),
                " AND u.email IS NOT NULL AND u.email_verified_at IS NOT NULL
                  AND (a.level = 'critical'
                       OR COALESCE((up.communication->'categories'->'announcements'->>'email')::boolean, TRUE))"
            ))
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
            // Queued in the claiming transaction: a failure here leaves the
            // announcement undelivered (retried) instead of marked delivered
            // with its e-mails lost.
            let emails: Vec<OutgoingEmail> = recipients
                .into_iter()
                .map(|recipient| OutgoingEmail {
                    user_id: Some(recipient.user_id),
                    to: recipient.email,
                    locale: recipient.language,
                    template: EmailTemplate::Announcement {
                        title: announcement.title.clone(),
                        body: announcement.body.clone(),
                        level: announcement.level.key().to_string(),
                        cta_label: announcement.cta_label.clone(),
                        cta_url: announcement.cta_url.clone(),
                    },
                })
                .collect();
            result.emails = enqueue_many_in(&mut tx, &emails).await?;
        }

        tx.commit().await?;
        Ok(Some((announcement, result)))
    }
}
