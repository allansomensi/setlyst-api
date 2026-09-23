//! Subscription operations that must be atomic: granting plan time,
//! trials, credits, promo codes, rewards and referral payouts.
//!
//! Every change to a subscription writes a `subscription_events` row in
//! the same transaction. Notifications are returned to the caller and sent
//! after the commit, so nobody is told about a change that rolled back.

use crate::{
    database::{
        AppState,
        repositories::billing_repository::{load_effective_plan, load_settings},
    },
    errors::api_error::{ApiError, codes},
    models::{
        billing::{
            BillingInterval, BillingMe, BillingSettings, CreditsSummary, PAYMENT_GRACE_DAYS,
            PromoKind, RedemptionSummary, ReferralSummary, Subscription, SubscriptionSource,
            SubscriptionStatus, subscription_in_effect,
        },
        notification::Notification,
    },
    services::{entitlements, notifier::notify_all},
};
use axum::http::StatusCode;
use chrono::{Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use tracing::{info, warn};
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Namespace mixed into per-user advisory lock keys, so they can't clash
/// with other advisory locks.
const CREDIT_LOCK_NAMESPACE: i64 = 0x5e71_7c4e_d175_0001;

/// Serializes credit operations of one user until the transaction ends.
pub async fn lock_user_credits(conn: &mut PgConnection, user_id: Uuid) -> Result<(), ApiError> {
    let bytes = user_id.as_bytes();
    let key = i64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]) ^ CREDIT_LOCK_NAMESPACE;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, FromRow)]
struct LockedSubscription {
    plan_code: String,
    status: SubscriptionStatus,
    source: SubscriptionSource,
    current_period_end: Option<NaiveDateTime>,
    trial_ends_at: Option<NaiveDateTime>,
    cancel_at_period_end: bool,
    external_ref: Option<String>,
    billing_interval: Option<String>,
}

impl LockedSubscription {
    fn is_effective(&self, at: NaiveDateTime) -> bool {
        subscription_in_effect(self.status, self.source, self.current_period_end, at)
    }

    /// A paid subscription the payment provider is still charging for.
    fn is_paid_and_effective(&self, at: NaiveDateTime) -> bool {
        self.source == SubscriptionSource::Payment && self.is_effective(at)
    }
}

/// Refused while a paid subscription is in effect: granting plan time
/// would replace a plan the provider keeps charging for.
fn paid_subscription_active() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::PAID_SUBSCRIPTION_ACTIVE,
        "This account has a paid subscription. Change or cancel it in the billing portal first.",
    )
}

async fn lock_subscription(
    conn: &mut PgConnection,
    user_id: Uuid,
) -> Result<Option<LockedSubscription>, ApiError> {
    Ok(sqlx::query_as::<_, LockedSubscription>(
        "SELECT plan_code, status, source, current_period_end, trial_ends_at, cancel_at_period_end,
                external_ref, billing_interval
         FROM subscriptions WHERE user_id = $1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn ensure_plan_exists(conn: &mut PgConnection, plan_code: &str) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM plans WHERE code = $1)")
        .bind(plan_code)
        .fetch_one(&mut *conn)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(ApiError::rule(
            StatusCode::NOT_FOUND,
            codes::PLAN_NOT_FOUND,
            "This plan does not exist.",
        ))
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_event(
    conn: &mut PgConnection,
    user_id: Uuid,
    kind: &str,
    from: Option<&LockedSubscription>,
    to_plan: Option<&str>,
    to_status: Option<SubscriptionStatus>,
    data: Value,
    actor_id: Option<Uuid>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO subscription_events (id, user_id, kind, from_plan, to_plan, from_status, to_status,
                                          data, actor_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(kind)
    .bind(from.map(|s| s.plan_code.clone()))
    .bind(to_plan)
    .bind(from.map(|s| s.status))
    .bind(to_status)
    .bind(data)
    .bind(actor_id)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The result of a subscription change.
#[derive(Debug, Clone)]
pub struct GrantOutcome {
    /// `plan_granted`, `extended`, `trial_started`, `trial_extended`.
    pub kind: &'static str,
    pub plan_code: String,
    pub status: SubscriptionStatus,
    pub current_period_end: Option<NaiveDateTime>,
}

impl GrantOutcome {
    pub fn notification(&self, user_id: Uuid) -> Notification {
        Notification::subscription_changed(
            user_id,
            self.kind,
            Some(&self.plan_code),
            Some(self.status.key()),
            self.current_period_end,
        )
    }
}

/// Grants `days` of `plan_code` (`None` = open-ended). Extends the current
/// period when the same plan is already in effect; otherwise switches the
/// account to the plan starting now.
#[allow(clippy::too_many_arguments)]
pub async fn grant_plan_time(
    conn: &mut PgConnection,
    user_id: Uuid,
    plan_code: &str,
    days: Option<i64>,
    source: SubscriptionSource,
    actor_id: Option<Uuid>,
    note: Option<&str>,
    reference: Value,
) -> Result<GrantOutcome, ApiError> {
    ensure_plan_exists(conn, plan_code).await?;
    let current = lock_subscription(conn, user_id).await?;
    let timestamp = now();
    if current
        .as_ref()
        .is_some_and(|s| s.is_paid_and_effective(timestamp))
    {
        return Err(paid_subscription_active());
    }

    let same_plan = current
        .as_ref()
        .is_some_and(|s| s.is_effective(timestamp) && s.plan_code == plan_code);

    let (kind, period_end) = if same_plan {
        let existing_end = current.as_ref().and_then(|s| s.current_period_end);
        let end = match (days, existing_end) {
            (None, _) | (_, None) => None,
            (Some(days), Some(end)) => Some(end.max(timestamp) + Duration::days(days)),
        };
        ("extended", end)
    } else {
        ("plan_granted", days.map(|d| timestamp + Duration::days(d)))
    };

    if same_plan {
        sqlx::query(
            "UPDATE subscriptions
             SET current_period_end = $2, status = 'active', source = $3,
                 trial_ends_at = CASE WHEN status = 'trialing' THEN NULL ELSE trial_ends_at END,
                 note = COALESCE($4, note), updated_at = $5, updated_by = $6
             WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(period_end)
        .bind(source)
        .bind(note)
        .bind(timestamp)
        .bind(actor_id)
        .execute(&mut *conn)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                        trial_ends_at, cancel_at_period_end, note, created_at, updated_at, updated_by)
             VALUES ($1, $2, 'active', $3, $4, $5, NULL, FALSE, $6, $4, $4, $7)
             ON CONFLICT (user_id) DO UPDATE SET
                 plan_code = $2, status = 'active', source = $3, started_at = $4, current_period_end = $5,
                 trial_ends_at = NULL, cancel_at_period_end = FALSE, canceled_at = NULL,
                 note = $6, updated_at = $4, updated_by = $7, trial_reminder_sent_at = NULL",
        )
        .bind(user_id)
        .bind(plan_code)
        .bind(source)
        .bind(timestamp)
        .bind(period_end)
        .bind(note)
        .bind(actor_id)
        .execute(&mut *conn)
        .await?;
    }

    let mut data = json!({ "days": days, "source": source, "note": note });
    if let (Some(obj), Some(extra)) = (data.as_object_mut(), reference.as_object()) {
        obj.extend(extra.clone());
    }
    record_event(
        conn,
        user_id,
        kind,
        current.as_ref(),
        Some(plan_code),
        Some(SubscriptionStatus::Active),
        data,
        actor_id,
    )
    .await?;

    Ok(GrantOutcome {
        kind,
        plan_code: plan_code.to_string(),
        status: SubscriptionStatus::Active,
        current_period_end: period_end,
    })
}

/// Starts a trial of `plan_code` for `days`, replacing an expired or
/// canceled subscription. Returns `None` when the account already has a
/// subscription in effect.
pub async fn start_trial(
    conn: &mut PgConnection,
    user_id: Uuid,
    plan_code: &str,
    days: i64,
    actor_id: Option<Uuid>,
) -> Result<Option<GrantOutcome>, ApiError> {
    ensure_plan_exists(conn, plan_code).await?;
    let current = lock_subscription(conn, user_id).await?;
    let timestamp = now();
    if current.as_ref().is_some_and(|s| s.is_effective(timestamp)) {
        return Ok(None);
    }
    let ends_at = timestamp + Duration::days(days);
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                    trial_ends_at, cancel_at_period_end, created_at, updated_at, updated_by)
         VALUES ($1, $2, 'trialing', 'trial', $3, $4, $4, FALSE, $3, $3, $5)
         ON CONFLICT (user_id) DO UPDATE SET
             plan_code = $2, status = 'trialing', source = 'trial', started_at = $3, current_period_end = $4,
             trial_ends_at = $4, cancel_at_period_end = FALSE, canceled_at = NULL,
             updated_at = $3, updated_by = $5, trial_reminder_sent_at = NULL",
    )
    .bind(user_id)
    .bind(plan_code)
    .bind(timestamp)
    .bind(ends_at)
    .bind(actor_id)
    .execute(&mut *conn)
    .await?;
    record_event(
        conn,
        user_id,
        "trial_started",
        current.as_ref(),
        Some(plan_code),
        Some(SubscriptionStatus::Trialing),
        json!({ "days": days }),
        actor_id,
    )
    .await?;
    Ok(Some(GrantOutcome {
        kind: "trial_started",
        plan_code: plan_code.to_string(),
        status: SubscriptionStatus::Trialing,
        current_period_end: Some(ends_at),
    }))
}

/// Local status of a subscription in the payment provider's status.
/// `None` = grants nothing yet (a checkout whose first payment hasn't
/// gone through).
pub fn map_provider_status(status: &str) -> Option<SubscriptionStatus> {
    match status {
        // A provider-side trial is a paid subscription with a card on
        // file and the first charge scheduled: it renews by itself.
        "active" | "trialing" => Some(SubscriptionStatus::Active),
        "past_due" => Some(SubscriptionStatus::PastDue),
        "canceled" => Some(SubscriptionStatus::Canceled),
        "unpaid" | "paused" => Some(SubscriptionStatus::Expired),
        _ => None,
    }
}

/// A paid subscription as the payment provider reports it, in local terms.
#[derive(Debug, Clone)]
pub struct PaymentSnapshot {
    /// The provider's subscription id.
    pub external_ref: String,
    pub plan_code: String,
    pub interval: Option<BillingInterval>,
    /// See [`map_provider_status`].
    pub status: Option<SubscriptionStatus>,
    pub provider_status: String,
    pub current_period_end: Option<NaiveDateTime>,
    pub cancel_at_period_end: bool,
    pub ended_at: Option<NaiveDateTime>,
}

/// What mirroring a paid subscription changed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentChange {
    /// `subscribed`, `plan_changed`, `payment_failed`, `payment_recovered`,
    /// `cancel_scheduled`, `resumed`, `renewed` or `canceled`.
    pub kind: &'static str,
    pub plan_code: String,
    pub status: SubscriptionStatus,
    pub current_period_end: Option<NaiveDateTime>,
}

impl PaymentChange {
    /// The notice for the account owner. Routine renewals and recoveries
    /// get none: the provider already e-mails the receipt.
    pub fn notification(&self, user_id: Uuid) -> Option<Notification> {
        matches!(
            self.kind,
            "subscribed"
                | "plan_changed"
                | "payment_failed"
                | "cancel_scheduled"
                | "resumed"
                | "canceled"
        )
        .then(|| {
            Notification::subscription_changed(
                user_id,
                self.kind,
                Some(&self.plan_code),
                Some(self.status.key()),
                self.current_period_end,
            )
        })
    }
}

/// Mirrors a paid subscription into `subscriptions` (the provider is the
/// source of truth for these). Returns what changed, or `None` when there
/// was nothing to apply.
///
/// Idempotent: applying the same snapshot twice changes nothing the second
/// time. A snapshot of a subscription that isn't the account's current one
/// only matters while it is live (a newer purchase); an old subscription
/// ending never touches the current row.
pub async fn apply_payment_subscription(
    conn: &mut PgConnection,
    user_id: Uuid,
    snapshot: &PaymentSnapshot,
) -> Result<Option<PaymentChange>, ApiError> {
    let Some(status) = snapshot.status else {
        return Ok(None);
    };
    ensure_plan_exists(conn, &snapshot.plan_code).await?;
    let current = lock_subscription(conn, user_id).await?;
    let timestamp = now();
    let live = status.is_live();
    let same = current.as_ref().is_some_and(|c| {
        c.source == SubscriptionSource::Payment
            && c.external_ref.as_deref() == Some(snapshot.external_ref.as_str())
    });
    if !same && !live {
        return Ok(None);
    }

    let period_end = if live {
        snapshot.current_period_end
    } else {
        Some(snapshot.ended_at.unwrap_or(timestamp))
    };
    let interval = snapshot.interval.map(|i| i.key().to_string());

    let kind = match current.as_ref().filter(|_| same) {
        None => Some("subscribed"),
        Some(c) => {
            let was_live = c.status.is_live();
            if !was_live && !live {
                None
            } else if was_live && !live {
                Some("canceled")
            } else if !was_live && live {
                Some("subscribed")
            } else if c.plan_code != snapshot.plan_code || c.billing_interval != interval {
                Some("plan_changed")
            } else if c.status == SubscriptionStatus::Active
                && status == SubscriptionStatus::PastDue
            {
                Some("payment_failed")
            } else if c.status == SubscriptionStatus::PastDue
                && status == SubscriptionStatus::Active
            {
                Some("payment_recovered")
            } else if !c.cancel_at_period_end && snapshot.cancel_at_period_end {
                Some("cancel_scheduled")
            } else if c.cancel_at_period_end && !snapshot.cancel_at_period_end {
                Some("resumed")
            } else if period_end > c.current_period_end {
                Some("renewed")
            } else {
                None
            }
        }
    };
    let Some(kind) = kind else {
        return Ok(None);
    };

    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                    trial_ends_at, cancel_at_period_end, canceled_at, external_ref,
                                    billing_interval, created_at, updated_at, updated_by)
         VALUES ($1, $2, $3, 'payment', $4, $5, NULL, $6, $7, $8, $9, $4, $4, NULL)
         ON CONFLICT (user_id) DO UPDATE SET
             plan_code = $2, status = $3, source = 'payment',
             started_at = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                               OR subscriptions.source <> 'payment'
                               THEN $4 ELSE subscriptions.started_at END,
             current_period_end = $5, trial_ends_at = NULL, cancel_at_period_end = $6,
             canceled_at = $7, external_ref = $8, billing_interval = $9, note = NULL,
             updated_at = $4, updated_by = NULL, trial_reminder_sent_at = NULL",
    )
    .bind(user_id)
    .bind(&snapshot.plan_code)
    .bind(status)
    .bind(timestamp)
    .bind(period_end)
    .bind(snapshot.cancel_at_period_end)
    .bind((!live).then_some(timestamp))
    .bind(&snapshot.external_ref)
    .bind(&interval)
    .execute(&mut *conn)
    .await?;

    record_event(
        conn,
        user_id,
        kind,
        current.as_ref(),
        Some(&snapshot.plan_code),
        Some(status),
        json!({
            "source": SubscriptionSource::Payment,
            "external_ref": snapshot.external_ref,
            "interval": interval,
            "provider_status": snapshot.provider_status,
        }),
        None,
    )
    .await?;

    Ok(Some(PaymentChange {
        kind,
        plan_code: snapshot.plan_code.clone(),
        status,
        current_period_end: period_end,
    }))
}

/// Adds `days` to a running trial, or starts a trial of the configured
/// trial plan when the account has no subscription in effect. Fails with
/// `PROMO_CODE_NOT_ELIGIBLE` for an account on a non-trial plan.
pub async fn extend_trial(
    conn: &mut PgConnection,
    user_id: Uuid,
    days: i64,
    settings: &BillingSettings,
) -> Result<GrantOutcome, ApiError> {
    let current = lock_subscription(conn, user_id).await?;
    let timestamp = now();
    match current {
        Some(sub) if sub.is_paid_and_effective(timestamp) => Err(paid_subscription_active()),
        Some(sub) if sub.is_effective(timestamp) && sub.status == SubscriptionStatus::Trialing => {
            let base = sub
                .trial_ends_at
                .or(sub.current_period_end)
                .unwrap_or(timestamp)
                .max(timestamp);
            let ends_at = base + Duration::days(days);
            sqlx::query(
                "UPDATE subscriptions SET trial_ends_at = $2, current_period_end = $2, updated_at = $3,
                        trial_reminder_sent_at = NULL
                 WHERE user_id = $1",
            )
            .bind(user_id)
            .bind(ends_at)
            .bind(timestamp)
            .execute(&mut *conn)
            .await?;
            record_event(
                conn,
                user_id,
                "trial_extended",
                Some(&sub),
                Some(&sub.plan_code),
                Some(SubscriptionStatus::Trialing),
                json!({ "days": days }),
                None,
            )
            .await?;
            Ok(GrantOutcome {
                kind: "trial_extended",
                plan_code: sub.plan_code,
                status: SubscriptionStatus::Trialing,
                current_period_end: Some(ends_at),
            })
        }
        Some(sub) if sub.is_effective(timestamp) => Err(ApiError::rule(
            StatusCode::FORBIDDEN,
            codes::PROMO_CODE_NOT_ELIGIBLE,
            "This code only applies to trials, and your account already has a plan.",
        )),
        _ => start_trial(conn, user_id, &settings.trial_plan, days, None)
            .await?
            .ok_or_else(|| {
                ApiError::rule(
                    StatusCode::FORBIDDEN,
                    codes::PROMO_CODE_NOT_ELIGIBLE,
                    "This code can't be applied to your account.",
                )
            }),
    }
}

/// Appends a ledger entry.
#[allow(clippy::too_many_arguments)]
pub async fn add_credits(
    conn: &mut PgConnection,
    user_id: Uuid,
    amount: i32,
    reason: &str,
    reference_id: Option<Uuid>,
    note: Option<&str>,
    actor_id: Option<Uuid>,
) -> Result<(), ApiError> {
    if amount == 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO credit_ledger (id, user_id, amount, reason, reference_id, note, actor_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(amount)
    .bind(reason)
    .bind(reference_id)
    .bind(note)
    .bind(actor_id)
    .bind(now())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn credit_balance(conn: &mut PgConnection, user_id: Uuid) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0)::bigint FROM credit_ledger WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?)
}

fn promo_error(code: &'static str, status: StatusCode, message: &str) -> ApiError {
    ApiError::rule(status, code, message)
}

#[derive(Debug, FromRow)]
struct PromoRow {
    id: Uuid,
    kind: PromoKind,
    plan_code: Option<String>,
    duration_days: Option<i32>,
    credits: Option<i32>,
    discount_percent: Option<i32>,
    new_users_only: bool,
    starts_at: Option<NaiveDateTime>,
    expires_at: Option<NaiveDateTime>,
    disabled_at: Option<NaiveDateTime>,
    created_at: NaiveDateTime,
}

/// Redeems promo code `code` for `user_id` and applies it.
pub async fn redeem_promo_code(
    state: &AppState,
    user_id: Uuid,
    code: &str,
) -> Result<RedemptionSummary, ApiError> {
    let normalized = code.trim().to_uppercase();
    let settings = load_settings(&state.db).await?;
    let mut tx = state.db.begin().await?;
    let timestamp = now();

    let promo: Option<PromoRow> = sqlx::query_as(
        "SELECT id, kind, plan_code, duration_days, credits, discount_percent, new_users_only,
                starts_at, expires_at, disabled_at, created_at
         FROM promo_codes WHERE code = $1",
    )
    .bind(&normalized)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(promo) = promo.filter(|p| p.disabled_at.is_none()) else {
        return Err(promo_error(
            codes::PROMO_CODE_INVALID,
            StatusCode::BAD_REQUEST,
            "This code is not valid.",
        ));
    };
    if promo.starts_at.is_some_and(|start| start > timestamp) {
        return Err(promo_error(
            codes::PROMO_CODE_INVALID,
            StatusCode::BAD_REQUEST,
            "This code is not valid yet.",
        ));
    }
    if promo.expires_at.is_some_and(|end| end <= timestamp) {
        return Err(promo_error(
            codes::PROMO_CODE_EXPIRED,
            StatusCode::BAD_REQUEST,
            "This code has expired.",
        ));
    }
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM promo_redemptions WHERE promo_code_id = $1 AND user_id = $2)",
    )
    .bind(promo.id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if already {
        return Err(promo_error(
            codes::PROMO_CODE_ALREADY_REDEEMED,
            StatusCode::CONFLICT,
            "You have already used this code.",
        ));
    }
    if promo.new_users_only {
        let created_at: NaiveDateTime =
            sqlx::query_scalar("SELECT created_at FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
        if created_at < promo.starts_at.unwrap_or(promo.created_at) {
            return Err(promo_error(
                codes::PROMO_CODE_NOT_ELIGIBLE,
                StatusCode::FORBIDDEN,
                "This code is only for new accounts.",
            ));
        }
    }

    // Atomic: concurrent redemptions of the last slot can't both win.
    let counted: Option<i32> = sqlx::query_scalar(
        "UPDATE promo_codes SET redemptions_count = redemptions_count + 1, updated_at = $2
         WHERE id = $1 AND (max_redemptions IS NULL OR redemptions_count < max_redemptions)
         RETURNING redemptions_count",
    )
    .bind(promo.id)
    .bind(timestamp)
    .fetch_optional(&mut *tx)
    .await?;
    if counted.is_none() {
        return Err(promo_error(
            codes::PROMO_CODE_EXHAUSTED,
            StatusCode::CONFLICT,
            "This code has reached its usage limit.",
        ));
    }

    let inserted = sqlx::query(
        "INSERT INTO promo_redemptions (id, promo_code_id, user_id, redeemed_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(promo.id)
    .bind(user_id)
    .bind(timestamp)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        return Err(match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => promo_error(
                codes::PROMO_CODE_ALREADY_REDEEMED,
                StatusCode::CONFLICT,
                "You have already used this code.",
            ),
            _ => ApiError::DatabaseError(e),
        });
    }

    let mut notifications = Vec::new();
    let reference = json!({ "promo_code_id": promo.id, "promo_code": normalized });
    match promo.kind {
        PromoKind::PlanGrant => {
            let plan = promo.plan_code.clone().unwrap_or_default();
            let outcome = grant_plan_time(
                &mut tx,
                user_id,
                &plan,
                promo.duration_days.map(i64::from),
                SubscriptionSource::PromoCode,
                None,
                None,
                reference,
            )
            .await?;
            notifications.push(outcome.notification(user_id));
        }
        PromoKind::TrialExtension => {
            let outcome = extend_trial(
                &mut tx,
                user_id,
                promo.duration_days.map(i64::from).unwrap_or(0).max(1),
                &settings,
            )
            .await?;
            notifications.push(outcome.notification(user_id));
        }
        PromoKind::Credits => {
            let amount = promo.credits.unwrap_or(0);
            lock_user_credits(&mut tx, user_id).await?;
            add_credits(
                &mut tx,
                user_id,
                amount,
                "promo_code",
                Some(promo.id),
                None,
                None,
            )
            .await?;
            notifications.push(Notification::credits_granted(user_id, amount, "promo_code"));
        }
        // Stored as the redemption itself; the payment integration applies
        // it to the next charge.
        PromoKind::Discount => {}
    }

    tx.commit().await?;
    notify_all(state, notifications).await;
    info!(%user_id, code = %normalized, "Promo code redeemed");

    Ok(RedemptionSummary {
        kind: promo.kind,
        plan_code: promo.plan_code,
        days: promo.duration_days,
        credits: promo.credits,
        discount_percent: promo.discount_percent,
    })
}

/// Spends credits on reward `reward_id` (plan time).
pub async fn redeem_reward(
    state: &AppState,
    user_id: Uuid,
    reward_id: &str,
) -> Result<(), ApiError> {
    let settings = load_settings(&state.db).await?;
    let Some(reward) = settings.rewards.iter().find(|r| r.id == reward_id).cloned() else {
        return Err(ApiError::rule(
            StatusCode::NOT_FOUND,
            codes::REWARD_NOT_FOUND,
            "This reward does not exist.",
        ));
    };
    let mut tx = state.db.begin().await?;
    lock_user_credits(&mut tx, user_id).await?;
    let balance = credit_balance(&mut tx, user_id).await?;
    if balance < i64::from(reward.cost) {
        return Err(ApiError::rule_with_meta(
            StatusCode::CONFLICT,
            codes::INSUFFICIENT_CREDITS,
            "You don't have enough credits for this reward.",
            json!({ "balance": balance, "required": reward.cost }),
        ));
    }
    add_credits(
        &mut tx,
        user_id,
        -reward.cost,
        "reward_redemption",
        None,
        Some(&reward.id),
        None,
    )
    .await?;
    let outcome = grant_plan_time(
        &mut tx,
        user_id,
        &reward.plan,
        Some(reward.days),
        SubscriptionSource::Credits,
        None,
        None,
        json!({ "reward_id": reward.id, "cost": reward.cost }),
    )
    .await?;
    tx.commit().await?;
    notify_all(state, vec![outcome.notification(user_id)]).await;
    Ok(())
}

/// Starts the registration trial when plans are enforced.
pub async fn start_registration_trial(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let settings = load_settings(&state.db).await?;
    if !settings.enforced || settings.trial_days <= 0 {
        return Ok(());
    }
    let mut tx = state.db.begin().await?;
    let outcome = start_trial(
        &mut tx,
        user_id,
        &settings.trial_plan,
        settings.trial_days,
        None,
    )
    .await;
    match outcome {
        Ok(outcome) => {
            tx.commit().await?;
            if let Some(outcome) = outcome {
                notify_all(state, vec![outcome.notification(user_id)]).await;
            }
        }
        // A misconfigured trial plan must not break sign-up.
        Err(e) => warn!(%user_id, error = %e, "Could not start the registration trial"),
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct PendingReferral {
    id: Uuid,
    referrer_id: Uuid,
}

async fn registration_ip(state: &AppState, user_id: Uuid) -> Result<Option<String>, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT ip_address FROM audit_logs
         WHERE action = 'user.registered' AND target_id = $1 AND ip_address IS NOT NULL
         ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?)
}

async fn reject_referral(state: &AppState, id: Uuid, note: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE referrals SET status = 'rejected', note = $2 WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(note)
    .execute(&state.db)
    .await?;
    info!(referral_id = %id, note, "Referral rejected");
    Ok(())
}

/// Called when `referred_id` verifies their e-mail: pays out (or rejects)
/// their pending referral.
pub async fn qualify_referral(state: &AppState, referred_id: Uuid) -> Result<(), ApiError> {
    let Some(referral) = sqlx::query_as::<_, PendingReferral>(
        "SELECT id, referrer_id FROM referrals WHERE referred_id = $1 AND status = 'pending'",
    )
    .bind(referred_id)
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };

    let settings = load_settings(&state.db).await?;
    if !settings.referral.enabled {
        return reject_referral(state, referral.id, "referral_disabled").await;
    }
    if referral.referrer_id == referred_id {
        return reject_referral(state, referral.id, "self_referral").await;
    }
    let (referrer_ip, referred_ip) = (
        registration_ip(state, referral.referrer_id).await?,
        registration_ip(state, referred_id).await?,
    );
    if let (Some(a), Some(b)) = (&referrer_ip, &referred_ip)
        && a == b
    {
        return reject_referral(state, referral.id, "same_registration_ip").await;
    }

    let mut tx = state.db.begin().await?;
    lock_user_credits(&mut tx, referral.referrer_id).await?;
    let rewarded_this_month: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM referrals
         WHERE referrer_id = $1 AND status = 'rewarded'
           AND rewarded_at >= date_trunc('month', (NOW() AT TIME ZONE 'utc'))",
    )
    .bind(referral.referrer_id)
    .fetch_one(&mut *tx)
    .await?;
    if rewarded_this_month >= settings.referral.max_rewarded_per_month {
        drop(tx);
        return reject_referral(state, referral.id, "monthly_limit_reached").await;
    }
    let claimed = sqlx::query(
        "UPDATE referrals SET status = 'rewarded', rewarded_at = $2 WHERE id = $1 AND status = 'pending'",
    )
    .bind(referral.id)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let (referrer_credits, referred_credits) = (
        settings.referral.referrer_credits,
        settings.referral.referred_credits,
    );
    add_credits(
        &mut tx,
        referral.referrer_id,
        referrer_credits,
        "referral_referrer",
        Some(referral.id),
        None,
        None,
    )
    .await?;
    add_credits(
        &mut tx,
        referred_id,
        referred_credits,
        "referral_referred",
        Some(referral.id),
        None,
        None,
    )
    .await?;
    tx.commit().await?;

    let mut notifications = Vec::new();
    if referrer_credits > 0 {
        notifications.push(Notification::credits_granted(
            referral.referrer_id,
            referrer_credits,
            "referral_referrer",
        ));
    }
    if referred_credits > 0 {
        notifications.push(Notification::credits_granted(
            referred_id,
            referred_credits,
            "referral_referred",
        ));
    }
    notify_all(state, notifications).await;
    info!(referral_id = %referral.id, "Referral rewarded");
    Ok(())
}

/// Ends the subscription of `user_id` now (staff action).
pub async fn revoke_subscription(
    state: &AppState,
    user_id: Uuid,
    actor_id: Uuid,
) -> Result<bool, ApiError> {
    // A paid subscription is canceled at the provider first: ending it
    // only here would leave the card being charged.
    crate::services::payments::cancel_paid_subscription(state, user_id).await?;

    let mut tx = state.db.begin().await?;
    let Some(current) = lock_subscription(&mut tx, user_id).await? else {
        return Ok(false);
    };
    let timestamp = now();
    sqlx::query(
        "UPDATE subscriptions SET status = 'expired', current_period_end = $2, updated_at = $2, updated_by = $3
         WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(timestamp)
    .bind(actor_id)
    .execute(&mut *tx)
    .await?;
    record_event(
        &mut tx,
        user_id,
        "revoked",
        Some(&current),
        Some(&current.plan_code),
        Some(SubscriptionStatus::Expired),
        json!({}),
        Some(actor_id),
    )
    .await?;
    tx.commit().await?;
    notify_all(
        state,
        vec![Notification::subscription_changed(
            user_id,
            "revoked",
            Some(&current.plan_code),
            Some("expired"),
            Some(timestamp),
        )],
    )
    .await;
    Ok(true)
}

/// Starts a `days`-day trial of the trial plan for every account without a
/// subscription. Returns how many trials were started.
pub async fn grant_trials(state: &AppState, days: i64, actor_id: Uuid) -> Result<i64, ApiError> {
    let settings = load_settings(&state.db).await?;
    let mut tx = state.db.begin().await?;
    ensure_plan_exists(&mut tx, &settings.trial_plan).await?;
    let timestamp = now();
    let ends_at = timestamp + Duration::days(days);
    let users: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                    trial_ends_at, cancel_at_period_end, created_at, updated_at, updated_by)
         SELECT u.id, $1, 'trialing', 'trial', $2, $3, $3, FALSE, $2, $2, $4
         FROM users u
         WHERE NOT EXISTS (SELECT 1 FROM subscriptions s WHERE s.user_id = u.id)
         RETURNING user_id",
    )
    .bind(&settings.trial_plan)
    .bind(timestamp)
    .bind(ends_at)
    .bind(actor_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO subscription_events (id, user_id, kind, to_plan, to_status, data, actor_id, created_at)
         SELECT gen_random_uuid(), t.user_id, 'trial_started', $2, 'trialing', $3, $4, $5
         FROM UNNEST($1::uuid[]) AS t(user_id)",
    )
    .bind(&users)
    .bind(&settings.trial_plan)
    .bind(json!({ "days": days, "bulk": true }))
    .bind(actor_id)
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    // In-app notice for everyone who didn't switch account messages off.
    sqlx::query(
        "INSERT INTO notifications (id, user_id, type, data, read_at, created_at)
         SELECT gen_random_uuid(), t.user_id, 'subscription_changed',
                jsonb_build_object('kind', 'trial_started', 'plan_code', $2::text, 'status', 'trialing',
                                   'current_period_end', $3::timestamp),
                NULL, $4
         FROM UNNEST($1::uuid[]) AS t(user_id)
         LEFT JOIN user_preferences up ON up.user_id = t.user_id
         WHERE COALESCE((up.communication->'categories'->'account'->>'in_app')::boolean, TRUE)",
    )
    .bind(&users)
    .bind(&settings.trial_plan)
    .bind(ends_at)
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(users.len() as i64)
}

/// Hourly maintenance while plans are enforced: expires subscriptions
/// whose period ended and sends trial-ending reminders three days ahead.
/// Returns `(expired, reminded)`.
///
/// Paid subscriptions are kept up to date by the payment provider's
/// webhook; they only expire here once their grace period has passed too
/// (the webhook never confirmed a renewal).
pub async fn run_subscription_maintenance(state: &AppState) -> Result<(u64, u64), ApiError> {
    let timestamp = now();
    // Webhook events are only redelivered for three days.
    sqlx::query("DELETE FROM stripe_events WHERE received_at < $1")
        .bind(timestamp - Duration::days(30))
        .execute(&state.db)
        .await?;

    let settings = load_settings(&state.db).await?;
    if !settings.enforced {
        return Ok((0, 0));
    }

    let expired: Vec<(Uuid, String, SubscriptionStatus)> = sqlx::query_as(
        "UPDATE subscriptions s SET status = 'expired', updated_at = $1
         FROM (SELECT user_id, status FROM subscriptions
               WHERE status IN ('trialing', 'active', 'past_due')
                 AND current_period_end IS NOT NULL AND current_period_end <= $1
                 AND (source <> 'payment' OR current_period_end <= $2)
               FOR UPDATE) AS old
         WHERE s.user_id = old.user_id
         RETURNING s.user_id, s.plan_code, old.status",
    )
    .bind(timestamp)
    .bind(timestamp - Duration::days(PAYMENT_GRACE_DAYS))
    .fetch_all(&state.db)
    .await?;
    let mut notifications = Vec::new();
    for (user_id, plan_code, from_status) in &expired {
        sqlx::query(
            "INSERT INTO subscription_events (id, user_id, kind, from_plan, to_plan, from_status, to_status,
                                              data, created_at)
             VALUES ($1, $2, 'expired', $3, $3, $4, 'expired', '{}', $5)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(plan_code)
        .bind(from_status)
        .bind(timestamp)
        .execute(&state.db)
        .await?;
        notifications.push(Notification::subscription_changed(
            *user_id,
            "expired",
            Some(plan_code),
            Some("expired"),
            Some(timestamp),
        ));
    }

    let reminders: Vec<(Uuid, String, NaiveDateTime)> = sqlx::query_as(
        "UPDATE subscriptions SET trial_reminder_sent_at = $1
         WHERE status = 'trialing' AND trial_reminder_sent_at IS NULL
           AND trial_ends_at > $1 AND trial_ends_at <= $2
         RETURNING user_id, plan_code, trial_ends_at",
    )
    .bind(timestamp)
    .bind(timestamp + Duration::days(3))
    .fetch_all(&state.db)
    .await?;
    for (user_id, plan_code, ends_at) in &reminders {
        notifications.push(Notification::trial_ending(*user_id, plan_code, *ends_at));
    }

    notify_all(state, notifications).await;
    Ok((expired.len() as u64, reminders.len() as u64))
}

/// The body of `GET /billing/me`.
pub async fn billing_me(state: &AppState, user_id: Uuid) -> Result<BillingMe, ApiError> {
    let settings = state.billing_repo.get_settings().await?;
    let entitlements = entitlements::entitlements(state, user_id).await?;
    let subscription: Option<Subscription> = state.billing_repo.get_subscription(user_id).await?;
    let balance = state.billing_repo.credit_balance(user_id).await?;
    let (rewarded, pending) = state.billing_repo.referral_counts(user_id).await?;
    let code: Option<String> = sqlx::query_scalar("SELECT referral_code FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .flatten();
    let plan = load_effective_plan(&state.db, user_id).await?;
    Ok(BillingMe {
        enforced: settings.enforced,
        payments_enabled: state.payments.is_some(),
        plan,
        subscription,
        features: entitlements.features(),
        credits: CreditsSummary { balance },
        referral: ReferralSummary {
            link_path: code.as_ref().map(|c| format!("/register?ref={c}")),
            code,
            rewarded_count: rewarded,
            pending_count: pending,
        },
        rewards: settings.rewards,
    })
}
