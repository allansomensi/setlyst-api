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
            BillingInterval, BillingMe, BillingSettings, CreditsSummary, PromoKind,
            RedemptionSummary, ReferralSummary, Subscription, SubscriptionSource,
            SubscriptionStatus, WITHDRAWAL_DAYS, subscription_in_effect,
        },
        notification::{Notification, NotificationType},
    },
    services::{
        entitlements::{self, Feature},
        notifier::notify_all,
        payments,
    },
};
use axum::http::StatusCode;
use chrono::{Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use tracing::{error, info, warn};
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
    past_due_since: Option<NaiveDateTime>,
    provider_status: Option<String>,
    unit_amount_cents: Option<i64>,
}

impl LockedSubscription {
    fn is_effective(&self, at: NaiveDateTime) -> bool {
        subscription_in_effect(
            self.status,
            self.source,
            self.current_period_end,
            self.past_due_since,
            at,
        )
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
                external_ref, billing_interval, past_due_since, provider_status, unit_amount_cents
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
pub(crate) async fn record_event_row(
    conn: &mut PgConnection,
    user_id: Uuid,
    kind: &str,
    to_plan: Option<&str>,
    data: Value,
    actor_id: Option<Uuid>,
) -> Result<(), ApiError> {
    record_event(conn, user_id, kind, None, to_plan, None, data, actor_id).await
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
                 note = $6, updated_at = $4, updated_by = $7, trial_reminder_sent_at = NULL,
                 past_due_since = NULL, provider_status = NULL, unit_amount_cents = NULL, currency = NULL,
                 paid_trial_reminder_sent_at = NULL, renewal_reminder_period_end = NULL,
                 terms_version = NULL, terms_accepted_at = NULL",
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
             updated_at = $3, updated_by = $5, trial_reminder_sent_at = NULL,
             past_due_since = NULL, provider_status = NULL, unit_amount_cents = NULL, currency = NULL,
             paid_trial_reminder_sent_at = NULL, renewal_reminder_period_end = NULL,
             terms_version = NULL, terms_accepted_at = NULL",
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
    /// End of a provider-side trial (card on file, first charge then).
    pub trial_end: Option<NaiveDateTime>,
    /// The price charged each period, minor units.
    pub unit_amount: Option<i64>,
    /// ISO 4217, upper case.
    pub currency: Option<String>,
}

/// What mirroring a paid subscription changed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaymentChange {
    /// `subscribed`, `plan_changed`, `payment_failed`, `payment_recovered`,
    /// `cancel_scheduled`, `resumed`, `renewed`, `updated`, `canceled`,
    /// `withdrawn`, `refunded` (by staff) or `disputed`.
    pub kind: &'static str,
    pub plan_code: String,
    pub status: SubscriptionStatus,
    pub current_period_end: Option<NaiveDateTime>,
    /// Extra fields for the notice (refunded amount...).
    pub extra: Value,
}

/// Paid-subscription notices that are billing messages: they reach the
/// inbox even when account e-mails are switched off (purchase and
/// cancellation confirmations, charge reminders; Decreto 7.962 art. 4).
pub const BILLING_NOTICE_KINDS: &[&str] = &[
    "subscribed",
    "plan_changed",
    "payment_failed",
    "cancel_scheduled",
    "resumed",
    "canceled",
    "withdrawn",
    "refunded",
    "disputed",
    "paid_trial_ending",
    "renewal_reminder",
    "price_change",
];

/// A billing notice about `user_id`'s paid subscription (`data` carries
/// the fields of `kind`).
pub fn billing_notice(user_id: Uuid, kind: &str, plan_code: &str, data: Value) -> Notification {
    let mut body = json!({ "kind": kind, "plan_code": plan_code });
    if let (Some(obj), Some(extra)) = (body.as_object_mut(), data.as_object()) {
        obj.extend(extra.clone());
    }
    Notification::new(user_id, NotificationType::SubscriptionChanged, body)
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
                | "withdrawn"
                | "refunded"
                | "disputed"
        )
        .then(|| {
            let mut notification = Notification::subscription_changed(
                user_id,
                self.kind,
                Some(&self.plan_code),
                Some(self.status.key()),
                self.current_period_end,
            );
            if let (Some(obj), Some(extra)) =
                (notification.data.as_object_mut(), self.extra.as_object())
            {
                obj.extend(extra.clone());
            }
            notification
        })
    }

    /// The account lost a paid subscription.
    pub fn ended(&self) -> bool {
        matches!(
            self.kind,
            "canceled" | "withdrawn" | "refunded" | "disputed"
        )
    }
}

/// What mirroring a paid subscription did.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Changed(PaymentChange),
    /// Nothing to apply (no change, or an old subscription ending).
    Unchanged,
    /// The account already has another paid subscription in effect
    /// (`current_ref`): this one is a second subscription and was not
    /// applied. The caller decides which one survives.
    Duplicate {
        current_ref: String,
    },
}

/// Mirrors a paid subscription into `subscriptions` (the provider is the
/// source of truth for these).
///
/// Idempotent: applying the same snapshot twice changes nothing the second
/// time. A snapshot of a subscription that isn't the account's current one
/// only matters while it is live, and never replaces another paid
/// subscription still in effect ([`Applied::Duplicate`]); an old
/// subscription ending never touches the current row.
pub async fn apply_payment_subscription(
    conn: &mut PgConnection,
    user_id: Uuid,
    snapshot: &PaymentSnapshot,
) -> Result<Applied, ApiError> {
    let Some(status) = snapshot.status else {
        return Ok(Applied::Unchanged);
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
        return Ok(Applied::Unchanged);
    }
    if !same
        && let Some(current_ref) = current
            .as_ref()
            .filter(|c| c.is_paid_and_effective(timestamp))
            .and_then(|c| c.external_ref.clone())
    {
        return Ok(Applied::Duplicate { current_ref });
    }

    let period_end = if live {
        snapshot.current_period_end
    } else {
        Some(snapshot.ended_at.unwrap_or(timestamp))
    };
    let interval = snapshot.interval.map(|i| i.key().to_string());
    let trial_end = snapshot
        .trial_end
        .filter(|_| snapshot.provider_status == "trialing");
    let past_due_since = (status == SubscriptionStatus::PastDue).then(|| {
        current
            .as_ref()
            .filter(|c| same && c.status == SubscriptionStatus::PastDue)
            .and_then(|c| c.past_due_since)
            .unwrap_or(timestamp)
    });

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
            } else if c.provider_status.as_deref() != Some(snapshot.provider_status.as_str())
                || c.unit_amount_cents != snapshot.unit_amount
                || c.trial_ends_at != trial_end
            {
                // A trial that started charging, a new price: no notice.
                Some("updated")
            } else {
                None
            }
        }
    };
    let Some(kind) = kind else {
        return Ok(Applied::Unchanged);
    };

    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                    trial_ends_at, cancel_at_period_end, canceled_at, external_ref,
                                    billing_interval, created_at, updated_at, updated_by,
                                    past_due_since, provider_status, unit_amount_cents, currency)
         VALUES ($1, $2, $3, 'payment', $4, $5, $10, $6, $7, $8, $9, $4, $4, NULL, $11, $12, $13, $14)
         ON CONFLICT (user_id) DO UPDATE SET
             plan_code = $2, status = $3, source = 'payment',
             started_at = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                               OR subscriptions.source <> 'payment'
                               THEN $4 ELSE subscriptions.started_at END,
             paid_trial_reminder_sent_at = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                                                  OR subscriptions.source <> 'payment'
                                                THEN NULL ELSE subscriptions.paid_trial_reminder_sent_at END,
             renewal_reminder_period_end = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                                                  OR subscriptions.source <> 'payment'
                                                THEN NULL ELSE subscriptions.renewal_reminder_period_end END,
             terms_version = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                                    OR subscriptions.source <> 'payment'
                                  THEN NULL ELSE subscriptions.terms_version END,
             terms_accepted_at = CASE WHEN subscriptions.external_ref IS DISTINCT FROM $8
                                        OR subscriptions.source <> 'payment'
                                      THEN NULL ELSE subscriptions.terms_accepted_at END,
             current_period_end = $5, trial_ends_at = $10, cancel_at_period_end = $6,
             canceled_at = $7, external_ref = $8, billing_interval = $9, note = NULL,
             updated_at = $4, updated_by = NULL, trial_reminder_sent_at = NULL,
             past_due_since = $11, provider_status = $12, unit_amount_cents = $13, currency = $14",
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
    .bind(trial_end)
    .bind(past_due_since)
    .bind(&snapshot.provider_status)
    .bind(snapshot.unit_amount)
    .bind(&snapshot.currency)
    .execute(&mut *conn)
    .await?;

    let mut data = json!({
        "source": SubscriptionSource::Payment,
        "external_ref": snapshot.external_ref,
        "interval": interval,
        "provider_status": snapshot.provider_status,
    });
    // A complimentary plan replaced by the purchase comes back when the
    // purchase ends (`restore_complimentary_grant`).
    if kind == "subscribed"
        && let Some(c) = current
            .as_ref()
            .filter(|c| !same && c.source == SubscriptionSource::Admin && c.is_effective(timestamp))
    {
        data["replaced"] = json!({
            "plan_code": c.plan_code,
            "source": c.source,
            "current_period_end": c.current_period_end,
        });
    }
    record_event(
        conn,
        user_id,
        kind,
        current.as_ref(),
        Some(&snapshot.plan_code),
        Some(status),
        data,
        None,
    )
    .await?;
    if kind == "canceled" {
        restore_complimentary_grant(conn, user_id, &snapshot.external_ref).await?;
    }

    Ok(Applied::Changed(PaymentChange {
        kind,
        plan_code: snapshot.plan_code.clone(),
        status,
        current_period_end: period_end,
        extra: Value::Null,
    }))
}

/// Ends the paid subscription `external_ref` of `user_id` here, right
/// away (it was canceled at the provider after a withdrawal, a dispute or
/// a full refund). `kind` is what the history and the notice call it.
/// `None` when that subscription isn't the account's live one.
pub async fn end_paid_subscription(
    conn: &mut PgConnection,
    user_id: Uuid,
    external_ref: &str,
    kind: &'static str,
    data: Value,
    actor_id: Option<Uuid>,
) -> Result<Option<PaymentChange>, ApiError> {
    let Some(current) = lock_subscription(conn, user_id).await?.filter(|c| {
        c.source == SubscriptionSource::Payment
            && c.external_ref.as_deref() == Some(external_ref)
            && c.status.is_live()
    }) else {
        return Ok(None);
    };
    let timestamp = now();
    sqlx::query(
        "UPDATE subscriptions
         SET status = 'canceled', current_period_end = $2, canceled_at = $2,
             cancel_at_period_end = FALSE, past_due_since = NULL, provider_status = 'canceled',
             updated_at = $2, updated_by = $3
         WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(timestamp)
    .bind(actor_id)
    .execute(&mut *conn)
    .await?;
    let mut event_data = json!({
        "source": SubscriptionSource::Payment,
        "external_ref": external_ref,
    });
    if let (Some(obj), Some(extra)) = (event_data.as_object_mut(), data.as_object()) {
        obj.extend(extra.clone());
    }
    record_event(
        conn,
        user_id,
        kind,
        Some(&current),
        Some(&current.plan_code),
        Some(SubscriptionStatus::Canceled),
        event_data,
        actor_id,
    )
    .await?;
    restore_complimentary_grant(conn, user_id, external_ref).await?;
    Ok(Some(PaymentChange {
        kind,
        plan_code: current.plan_code,
        status: SubscriptionStatus::Canceled,
        current_period_end: Some(timestamp),
        extra: data,
    }))
}

/// Brings back the complimentary (staff) plan a purchase replaced, when
/// that purchase ends and the grant would still be running. `true` when a
/// grant was restored.
async fn restore_complimentary_grant(
    conn: &mut PgConnection,
    user_id: Uuid,
    external_ref: &str,
) -> Result<bool, ApiError> {
    let replaced: Option<Value> = sqlx::query_scalar(
        "SELECT data->'replaced' FROM subscription_events
         WHERE user_id = $1 AND kind = 'subscribed' AND data->>'external_ref' = $2
           AND data ? 'replaced'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(external_ref)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(replaced) = replaced else {
        return Ok(false);
    };
    let Some(plan_code) = replaced["plan_code"].as_str() else {
        return Ok(false);
    };
    let until: Option<NaiveDateTime> =
        serde_json::from_value(replaced["current_period_end"].clone()).unwrap_or(None);
    let timestamp = now();
    if until.is_some_and(|end| end <= timestamp) {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE subscriptions
         SET plan_code = $2, status = 'active', source = 'admin', current_period_end = $3,
             started_at = $4, canceled_at = NULL, cancel_at_period_end = FALSE, external_ref = NULL,
             billing_interval = NULL, past_due_since = NULL, provider_status = NULL,
             unit_amount_cents = NULL, currency = NULL, trial_ends_at = NULL, updated_at = $4
         WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(plan_code)
    .bind(until)
    .bind(timestamp)
    .execute(&mut *conn)
    .await?;
    record_event(
        conn,
        user_id,
        "grant_restored",
        None,
        Some(plan_code),
        Some(SubscriptionStatus::Active),
        json!({ "source": SubscriptionSource::Admin, "after": external_ref }),
        None,
    )
    .await?;
    info!(%user_id, plan = plan_code, "Complimentary plan restored after a paid subscription ended");
    Ok(true)
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
///
/// Idempotent and safe to call again later (for example on e-mail
/// verification): an account gets at most one registration trial, and
/// never while or after it had any subscription.
pub async fn start_registration_trial(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let settings = load_settings(&state.db).await?;
    if !settings.enforced || settings.trial_days <= 0 {
        return Ok(());
    }
    let mut tx = state.db.begin().await?;
    // Two calls at once (sign-up and verification racing) wait here.
    lock_user_credits(&mut tx, user_id).await?;
    let had_plan: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM subscriptions WHERE user_id = $1)
             OR EXISTS (SELECT 1 FROM subscription_events
                        WHERE user_id = $1 AND kind = 'trial_started')",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;
    if had_plan {
        return Ok(());
    }
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

async fn pending_referral(
    state: &AppState,
    referred_id: Uuid,
) -> Result<Option<PendingReferral>, ApiError> {
    Ok(sqlx::query_as::<_, PendingReferral>(
        "SELECT id, referrer_id FROM referrals WHERE referred_id = $1 AND status = 'pending'",
    )
    .bind(referred_id)
    .fetch_optional(&state.db)
    .await?)
}

/// Whether two accounts deliver to the same mailbox once tags, dots and
/// case are folded (`me@gmail.com` referring `m.e+x@gmail.com`): the same
/// rule that keeps trials to one per person, applied to referrals.
async fn same_mailbox(state: &AppState, a: Uuid, b: Uuid) -> Result<bool, ApiError> {
    let emails: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, email FROM users WHERE id = $1 OR id = $2")
            .bind(a)
            .bind(b)
            .fetch_all(&state.db)
            .await?;
    let mailbox = |id: Uuid| {
        emails
            .iter()
            .find(|(user, _)| *user == id)
            .map(|(_, email)| crate::models::user::canonical_email(email))
    };
    Ok(matches!((mailbox(a), mailbox(b)), (Some(x), Some(y)) if x == y))
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

/// Called when `referred_id` verifies their e-mail: pays the referred
/// account its own welcome bonus (once). The referrer is paid later, on
/// that account's first paid invoice ([`reward_referrer_on_payment`]), so
/// throwaway sign-ups earn nothing.
pub async fn qualify_referral(state: &AppState, referred_id: Uuid) -> Result<(), ApiError> {
    let Some(referral) = pending_referral(state, referred_id).await? else {
        return Ok(());
    };
    let settings = load_settings(&state.db).await?;
    if !settings.referral.enabled {
        return reject_referral(state, referral.id, "referral_disabled").await;
    }
    if referral.referrer_id == referred_id
        || same_mailbox(state, referral.referrer_id, referred_id).await?
    {
        return reject_referral(state, referral.id, "self_referral").await;
    }

    let mut tx = state.db.begin().await?;
    lock_user_credits(&mut tx, referred_id).await?;
    let claimed = sqlx::query(
        "UPDATE referrals SET referred_rewarded_at = $2
         WHERE id = $1 AND status = 'pending' AND referred_rewarded_at IS NULL",
    )
    .bind(referral.id)
    .bind(now())
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let credits = settings.referral.referred_credits;
    add_credits(
        &mut tx,
        referred_id,
        credits,
        "referral_referred",
        Some(referral.id),
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    if credits > 0 {
        notify_all(
            state,
            vec![Notification::credits_granted(
                referred_id,
                credits,
                "referral_referred",
            )],
        )
        .await;
    }
    info!(referral_id = %referral.id, "Referred account bonus paid");
    Ok(())
}

/// Called when `referred_id` pays its first invoice: pays out (or
/// rejects, over the monthly limit) the referrer's reward.
pub async fn reward_referrer_on_payment(
    state: &AppState,
    referred_id: Uuid,
) -> Result<(), ApiError> {
    let Some(referral) = pending_referral(state, referred_id).await? else {
        return Ok(());
    };
    let settings = load_settings(&state.db).await?;
    if !settings.referral.enabled {
        return reject_referral(state, referral.id, "referral_disabled").await;
    }
    if referral.referrer_id == referred_id {
        return reject_referral(state, referral.id, "self_referral").await;
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
    let credits = settings.referral.referrer_credits;
    add_credits(
        &mut tx,
        referral.referrer_id,
        credits,
        "referral_referrer",
        Some(referral.id),
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    if credits > 0 {
        notify_all(
            state,
            vec![Notification::credits_granted(
                referral.referrer_id,
                credits,
                "referral_referrer",
            )],
        )
        .await;
    }
    info!(referral_id = %referral.id, "Referral rewarded");
    Ok(())
}

/// Days after a referral reward within which the referred account's
/// refunded (withdrawn, refunded by staff or disputed) first payment
/// takes the reward back.
pub const REFERRAL_CLAWBACK_DAYS: i64 = 30;

/// Called when the paid subscription of `referred_id` ends with a refund
/// or a dispute: a reward paid on that payment within
/// [`REFERRAL_CLAWBACK_DAYS`] is reversed (the referrer's credits are
/// debited, going negative if they were already spent, and the referral
/// is marked rejected), so a "referred" account that pays and withdraws
/// can't mint credits for its referrer.
pub async fn reverse_referral_reward(
    state: &AppState,
    referred_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    let mut tx = state.db.begin().await?;
    let Some((referral_id, referrer_id)): Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, referrer_id FROM referrals
         WHERE referred_id = $1 AND status = 'rewarded'
           AND rewarded_at >= $2
         FOR UPDATE",
    )
    .bind(referred_id)
    .bind(now() - chrono::Duration::days(REFERRAL_CLAWBACK_DAYS))
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(());
    };
    lock_user_credits(&mut tx, referrer_id).await?;
    let granted: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount), 0)::bigint FROM credit_ledger
         WHERE user_id = $1 AND reference_id = $2
           AND reason IN ('referral_referrer', 'referral_reversed')",
    )
    .bind(referrer_id)
    .bind(referral_id)
    .fetch_one(&mut *tx)
    .await?;
    let claimed = sqlx::query(
        "UPDATE referrals SET status = 'rejected', note = $2 WHERE id = $1 AND status = 'rewarded'",
    )
    .bind(referral_id)
    .bind(format!("reward_reversed:{reason}"))
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    if granted > 0 {
        add_credits(
            &mut tx,
            referrer_id,
            -(granted.min(i32::MAX as i64) as i32),
            "referral_reversed",
            Some(referral_id),
            Some(reason),
            None,
        )
        .await?;
    }
    tx.commit().await?;
    warn!(referral_id = %referral_id, %referrer_id, reversed = granted, reason, "Referral reward reversed");
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

/// Hourly maintenance.
///
/// Paid subscriptions (whatever the enforcement setting, since the
/// provider keeps charging them): those whose period ended or whose failed
/// renewal ran out of grace are reconciled with the provider before they
/// lose their plan; card-on-file trials get their "first charge in 7 days"
/// reminder, yearly plans their renewal reminder; customers whose deletion
/// failed are retried; once a day every subscription at the provider is
/// compared with its mirror.
///
/// While plans are enforced: expires the other subscriptions whose period
/// ended (revoking public links the account's plan no longer includes) and
/// sends trial-ending reminders three days ahead.
///
/// Returns `(expired, reminded)`.
pub async fn run_subscription_maintenance(state: &AppState) -> Result<(u64, u64), ApiError> {
    let timestamp = now();
    // Webhook events are only redelivered for three days.
    sqlx::query("DELETE FROM stripe_events WHERE received_at < $1")
        .bind(timestamp - Duration::days(30))
        .execute(&state.db)
        .await?;

    let mut expired_count = payments::reconcile_due(state).await?;
    let mut reminded = send_paid_trial_reminders(state, None).await?;
    reminded += send_renewal_reminders(state, None, RENEWAL_REMINDER_DAYS, None).await?;
    payments::process_cleanup_queue(state).await?;
    if claim_job_run(state, "stripe_reconciliation", Duration::hours(24)).await? {
        match payments::reconcile_all(state).await {
            Ok(count) => info!(count, "Stripe subscriptions reconciled"),
            Err(e) => error!(error = %e, "Stripe reconciliation failed"),
        }
    }

    let settings = load_settings(&state.db).await?;
    if !settings.enforced {
        return Ok((expired_count, reminded));
    }

    // Paid subscriptions are expired by `payments::reconcile_due`, after
    // asking the provider.
    let expired: Vec<(Uuid, String, SubscriptionStatus)> = sqlx::query_as(
        "UPDATE subscriptions s SET status = 'expired', updated_at = $1
         FROM (SELECT user_id, status FROM subscriptions
               WHERE status IN ('trialing', 'active', 'past_due')
                 AND current_period_end IS NOT NULL AND current_period_end <= $1
                 AND source <> 'payment'
               FOR UPDATE) AS old
         WHERE s.user_id = old.user_id
         RETURNING s.user_id, s.plan_code, old.status",
    )
    .bind(timestamp)
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
        revoke_shares_if_lost(state, *user_id).await?;
    }
    expired_count += expired.len() as u64;

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
    Ok((expired_count, reminded + reminders.len() as u64))
}

/// `true` when job `name` last ran more than `every` ago, recording this
/// run (at most one replica wins).
async fn claim_job_run(state: &AppState, name: &str, every: Duration) -> Result<bool, ApiError> {
    let timestamp = now();
    let claimed: Option<String> = sqlx::query_scalar(
        "INSERT INTO billing_job_runs (name, last_run_at) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET last_run_at = $2
         WHERE billing_job_runs.last_run_at <= $3
         RETURNING name",
    )
    .bind(name)
    .bind(timestamp)
    .bind(timestamp - every)
    .fetch_optional(&state.db)
    .await?;
    Ok(claimed.is_some())
}

/// How long before the first charge of a card-on-file trial the owner is
/// reminded (card-network rules; Subscription Terms §4.4).
pub const PAID_TRIAL_REMINDER_DAYS: i64 = 7;

/// How long before a yearly renewal the owner is reminded when the
/// provider's `invoice.upcoming` event didn't do it earlier (Subscription
/// Terms §6.2).
pub const RENEWAL_REMINDER_DAYS: i64 = 7;

/// Sends the "your first charge is on {date}" reminder of card-on-file
/// trials ending within [`PAID_TRIAL_REMINDER_DAYS`], once per
/// subscription. `user_id` limits it to one account. Returns how many were
/// sent.
pub async fn send_paid_trial_reminders(
    state: &AppState,
    user_id: Option<Uuid>,
) -> Result<u64, ApiError> {
    let timestamp = now();
    #[allow(clippy::type_complexity)]
    let due: Vec<(
        Uuid,
        String,
        NaiveDateTime,
        Option<i64>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "UPDATE subscriptions SET paid_trial_reminder_sent_at = $1
         WHERE source = 'payment' AND status = 'active' AND provider_status = 'trialing'
           AND paid_trial_reminder_sent_at IS NULL AND NOT cancel_at_period_end
           AND trial_ends_at > $1 AND trial_ends_at <= $2
           AND ($3::uuid IS NULL OR user_id = $3)
         RETURNING user_id, plan_code, trial_ends_at, unit_amount_cents, currency, billing_interval",
    )
    .bind(timestamp)
    .bind(timestamp + Duration::days(PAID_TRIAL_REMINDER_DAYS))
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let notifications = due
        .iter()
        .map(|(user_id, plan, charge_at, amount, currency, interval)| {
            billing_notice(
                *user_id,
                "paid_trial_ending",
                plan,
                json!({
                    "charge_at": charge_at,
                    "amount_cents": amount,
                    "currency": currency,
                    "interval": interval,
                }),
            )
        })
        .collect();
    notify_all(state, notifications).await;
    Ok(due.len() as u64)
}

/// Sends the yearly renewal reminder of subscriptions renewing within
/// `within_days`, once per period. `user_id` limits it to one account;
/// `amount` is the upcoming invoice's `(minor units, currency)` when the
/// provider announced it. Returns how many were sent.
pub async fn send_renewal_reminders(
    state: &AppState,
    user_id: Option<Uuid>,
    within_days: i64,
    amount: Option<(i64, String)>,
) -> Result<u64, ApiError> {
    let timestamp = now();
    #[allow(clippy::type_complexity)]
    let due: Vec<(Uuid, String, NaiveDateTime, Option<i64>, Option<String>)> = sqlx::query_as(
        "UPDATE subscriptions SET renewal_reminder_period_end = current_period_end
         WHERE source = 'payment' AND status = 'active' AND billing_interval = 'yearly'
           AND COALESCE(provider_status, 'active') = 'active' AND NOT cancel_at_period_end
           AND current_period_end > $1 AND current_period_end <= $2
           AND renewal_reminder_period_end IS DISTINCT FROM current_period_end
           AND ($3::uuid IS NULL OR user_id = $3)
         RETURNING user_id, plan_code, current_period_end, unit_amount_cents, currency",
    )
    .bind(timestamp)
    .bind(timestamp + Duration::days(within_days))
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let notifications = due
        .iter()
        .map(|(user_id, plan, renews_at, unit_amount, currency)| {
            let (amount_cents, currency) = match &amount {
                Some((cents, currency)) => (Some(*cents), Some(currency.clone())),
                None => (*unit_amount, currency.clone()),
            };
            billing_notice(
                *user_id,
                "renewal_reminder",
                plan,
                json!({
                    "renews_at": renews_at,
                    "amount_cents": amount_cents,
                    "currency": currency,
                    "interval": "yearly",
                }),
            )
        })
        .collect();
    notify_all(state, notifications).await;
    Ok(due.len() as u64)
}

/// New list prices of a plan, minor units (`None` = unchanged).
#[derive(Debug, Clone, Copy, Default)]
pub struct NewPrices {
    pub monthly_cents: Option<i64>,
    pub yearly_cents: Option<i64>,
}

/// Tells every paying subscriber of `plan_code` whose price changes that
/// it will cost `new_prices` from `effective_at` (Subscription Terms
/// §11.1: at least 30 days ahead). Only the notice: moving the
/// subscriptions to the new price is a separate step. Returns how many
/// subscribers were told.
pub async fn notify_price_change(
    state: &AppState,
    plan_code: &str,
    new_prices: NewPrices,
    effective_at: NaiveDateTime,
) -> Result<u64, ApiError> {
    #[allow(clippy::type_complexity)]
    let subscribers: Vec<(Uuid, Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT user_id, billing_interval, unit_amount_cents, currency FROM subscriptions
         WHERE source = 'payment' AND plan_code = $1 AND status IN ('active', 'past_due')
           AND NOT cancel_at_period_end",
    )
    .bind(plan_code)
    .fetch_all(&state.db)
    .await?;
    let mut notifications = Vec::new();
    for (user_id, interval, old_amount, currency) in subscribers {
        let new_amount = match interval.as_deref() {
            Some("yearly") => new_prices.yearly_cents,
            _ => new_prices.monthly_cents,
        };
        let Some(new_amount) = new_amount.filter(|n| Some(*n) != old_amount) else {
            continue;
        };
        let data = json!({
            "interval": interval,
            "old_amount_cents": old_amount,
            "new_amount_cents": new_amount,
            "currency": currency,
            "effective_at": effective_at,
        });
        let mut conn = state.db.acquire().await?;
        record_event_row(
            &mut conn,
            user_id,
            "price_change_notified",
            Some(plan_code),
            data.clone(),
            None,
        )
        .await?;
        notifications.push(billing_notice(user_id, "price_change", plan_code, data));
    }
    let count = notifications.len() as u64;
    notify_all(state, notifications).await;
    info!(plan = plan_code, count, "Price change notices sent");
    Ok(count)
}

/// Turns off the public links of `user_id`'s own setlists and gigs when
/// the account's plan no longer includes public sharing. Returns how many
/// links were revoked.
pub async fn revoke_shares_if_lost(state: &AppState, user_id: Uuid) -> Result<u64, ApiError> {
    if entitlements::has_feature(state, user_id, Feature::PublicSharing).await? {
        return Ok(0);
    }
    let setlists = sqlx::query(
        "UPDATE setlists SET share_token = NULL
         WHERE user_id = $1 AND band_id IS NULL AND share_token IS NOT NULL",
    )
    .bind(user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    let gigs = sqlx::query(
        "UPDATE gigs SET share_token = NULL
         WHERE user_id = $1 AND band_id IS NULL AND share_token IS NOT NULL",
    )
    .bind(user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if setlists + gigs > 0 {
        info!(%user_id, setlists, gigs, "Public links revoked: the plan no longer includes sharing");
    }
    Ok(setlists + gigs)
}

/// The last day the buyer may withdraw from a paid subscription whose
/// first paid invoice was paid at `first_paid` and the latest at
/// `latest_paid`: 7 days from the first charge, and, for yearly plans, 7
/// days from each renewal charge too (conservative reading of CDC art. 49).
pub fn withdrawal_deadline(
    first_paid: NaiveDateTime,
    latest_paid: NaiveDateTime,
    yearly: bool,
) -> NaiveDateTime {
    let first = first_paid + Duration::days(WITHDRAWAL_DAYS);
    if yearly {
        first.max(latest_paid + Duration::days(WITHDRAWAL_DAYS))
    } else {
        first
    }
}

/// Until when the account's current paid subscription can be withdrawn
/// from, per the payment ledger. `None` outside the window.
async fn withdrawal_eligible_until(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<NaiveDateTime>, ApiError> {
    let row: Option<(Option<NaiveDateTime>, Option<NaiveDateTime>, Option<String>)> =
        sqlx::query_as(
            "SELECT MIN(p.paid_at), MAX(p.paid_at), MAX(s.billing_interval)
             FROM subscriptions s
             JOIN payments p ON p.subscription_id = s.external_ref
             WHERE s.user_id = $1 AND s.source = 'payment'
               AND s.status IN ('active', 'past_due')
               AND p.amount_cents > 0 AND p.refunded_cents < p.amount_cents",
        )
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    let Some((Some(first), Some(latest), interval)) = row else {
        return Ok(None);
    };
    let deadline = withdrawal_deadline(first, latest, interval.as_deref() == Some("yearly"));
    Ok((deadline > now()).then_some(deadline))
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
    let past_due_since = subscription
        .as_ref()
        .filter(|s| {
            s.source == SubscriptionSource::Payment && s.status == SubscriptionStatus::PastDue
        })
        .and_then(|s| s.past_due_since)
        .map(|t| t.and_utc());
    let withdrawal_eligible_until = withdrawal_eligible_until(state, user_id)
        .await?
        .map(|t| t.and_utc());
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
        withdrawal_eligible_until,
        past_due_since,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    #[test]
    fn withdrawal_counts_from_the_first_charge_and_yearly_renewals() {
        assert_eq!(withdrawal_deadline(at(1), at(1), false), at(8));
        // A monthly renewal doesn't reopen the window.
        assert_eq!(withdrawal_deadline(at(1), at(20), false), at(8));
        // A yearly renewal does.
        assert_eq!(withdrawal_deadline(at(1), at(20), true), at(27));
    }

    #[test]
    fn billing_notices_carry_their_fields() {
        let user = Uuid::nil();
        let notice = billing_notice(
            user,
            "paid_trial_ending",
            "pro",
            json!({ "amount_cents": 3990 }),
        );
        assert_eq!(notice.data["kind"], "paid_trial_ending");
        assert_eq!(notice.data["plan_code"], "pro");
        assert_eq!(notice.data["amount_cents"], 3990);
        let change = PaymentChange {
            kind: "withdrawn",
            plan_code: "pro".into(),
            status: SubscriptionStatus::Canceled,
            current_period_end: None,
            extra: json!({ "refunded_cents": 3990 }),
        };
        assert!(change.ended());
        let notice = change.notification(user).unwrap();
        assert_eq!(notice.data["refunded_cents"], 3990);
        assert!(BILLING_NOTICE_KINDS.contains(&"withdrawn"));
    }
}
