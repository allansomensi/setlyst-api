//! Paid subscriptions: checkout, plan changes, the billing portal and the
//! payment provider's webhook.
//!
//! The provider is the source of truth for paid subscriptions. Every
//! webhook event is only a hint: the subscription it names is fetched fresh
//! and mirrored locally (`billing::apply_payment_subscription`), so events
//! may arrive late, twice or out of order without consequence. Each event
//! id is applied at most once, in the same transaction as its change.

use crate::{
    config::Config,
    database::AppState,
    errors::api_error::{ApiError, codes},
    models::{
        billing::{
            BillingInterval, BillingMe, CheckoutPayload, Plan, RedirectResponse,
            SubscriptionSource, SubscriptionStatus,
        },
        notification::Notification,
    },
    payments::{
        CheckoutRequest, GatewaySubscription, Payments, PriceSpec,
        webhook::{self, SignatureError},
    },
    services::{
        account::user_locale,
        billing::{self, PaymentSnapshot, apply_payment_subscription, map_provider_status},
        notifier::notify_all,
    },
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;
use validator::Validate;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// The provider refuses a trial ending sooner than this (Stripe: 48 h).
const MIN_TRIAL_HOURS: i64 = 49;

/// Webhook event types that can change a subscription.
const HANDLED_EVENTS: &[&str] = &[
    "checkout.session.completed",
    "checkout.session.async_payment_succeeded",
    "customer.subscription.created",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "customer.subscription.paused",
    "customer.subscription.resumed",
    "customer.subscription.pending_update_applied",
    "customer.subscription.pending_update_expired",
    "invoice.paid",
    "invoice.payment_failed",
];

fn payments(state: &AppState) -> Result<&Payments, ApiError> {
    state.payments.as_ref().ok_or_else(|| {
        ApiError::rule(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::PAYMENTS_UNAVAILABLE,
            "Payments are not available right now.",
        )
    })
}

fn unix_to_naive(seconds: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(seconds, 0).map(|d| d.naive_utc())
}

/// An absolute link into the web app.
fn app_url(locale: &str, path: &str) -> String {
    let base = Config::try_get()
        .map(|c| c.app_base_url.as_str())
        .unwrap_or("http://localhost:3000");
    format!("{}/{locale}{path}", base.trim_end_matches('/'))
}

#[derive(Debug, FromRow)]
struct Account {
    username: String,
    email: Option<String>,
    email_verified_at: Option<NaiveDateTime>,
    stripe_customer_id: Option<String>,
}

async fn load_account(state: &AppState, user_id: Uuid) -> Result<Account, ApiError> {
    sqlx::query_as(
        "SELECT username, email, email_verified_at, stripe_customer_id FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn ensure_enforced(state: &AppState) -> Result<(), ApiError> {
    if state.billing_repo.get_settings().await?.enforced {
        Ok(())
    } else {
        Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::BILLING_NOT_ENFORCED,
            "Every feature is free during the pre-release; there is nothing to pay for yet.",
        ))
    }
}

/// A public plan with a price for `interval`.
async fn purchasable_plan(
    state: &AppState,
    plan_code: &str,
    interval: BillingInterval,
) -> Result<Plan, ApiError> {
    let plan = state
        .billing_repo
        .find_plan(plan_code)
        .await?
        .filter(|p| p.is_public)
        .ok_or_else(|| {
            ApiError::rule(
                StatusCode::NOT_FOUND,
                codes::PLAN_NOT_FOUND,
                "This plan does not exist.",
            )
        })?;
    if interval.price_of(&plan) <= 0 {
        return Err(ApiError::rule(
            StatusCode::BAD_REQUEST,
            codes::PLAN_NOT_PURCHASABLE,
            "This plan can't be bought with this billing interval.",
        ));
    }
    Ok(plan)
}

fn price_spec(plan: &Plan, interval: BillingInterval) -> PriceSpec {
    let name = ["pt-BR", "en"]
        .iter()
        .find_map(|l| plan.name.get(l).and_then(Value::as_str))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(&plan.code);
    PriceSpec {
        plan_code: plan.code.clone(),
        product_name: format!("Setlyst {name}"),
        interval,
        currency: plan.currency.clone(),
        unit_amount: i64::from(interval.price_of(plan)),
    }
}

/// The provider customer of the account, created on first use.
async fn ensure_customer(
    state: &AppState,
    payments: &Payments,
    user_id: Uuid,
    account: &Account,
    locale: &str,
) -> Result<String, ApiError> {
    if let Some(id) = &account.stripe_customer_id {
        return Ok(id.clone());
    }
    let id = payments
        .gateway
        .create_customer(user_id, account.email.as_deref(), &account.username, locale)
        .await?;
    // A concurrent checkout may have stored one already: keep the first.
    let stored: String = sqlx::query_scalar(
        "UPDATE users SET stripe_customer_id = COALESCE(stripe_customer_id, $2)
         WHERE id = $1 RETURNING stripe_customer_id",
    )
    .bind(user_id)
    .bind(&id)
    .fetch_one(&state.db)
    .await?;
    Ok(stored)
}

/// The best discount available on the first charge of `plan_code`: a
/// running promotion or an unused `discount` promo code the account
/// redeemed (the redemption is returned so it can be spent). Promotions
/// win ties, keeping the code for later.
async fn best_discount(
    state: &AppState,
    user_id: Uuid,
    plan_code: &str,
) -> Result<Option<(i32, Option<Uuid>)>, ApiError> {
    let promotion = state
        .billing_repo
        .running_promotions()
        .await?
        .into_iter()
        .filter(|(plan, _)| plan.as_deref().is_none_or(|p| p == plan_code))
        .map(|(_, promotion)| promotion.discount_percent)
        .max();
    let redemption: Option<(Uuid, i32)> = sqlx::query_as(
        "SELECT r.id, p.discount_percent FROM promo_redemptions r
         JOIN promo_codes p ON p.id = r.promo_code_id
         WHERE r.user_id = $1 AND p.kind = 'discount' AND r.applied_at IS NULL
           AND p.discount_percent IS NOT NULL
         ORDER BY p.discount_percent DESC, r.redeemed_at
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(match (promotion, redemption) {
        (Some(p), Some((id, r))) if r > p => Some((r, Some(id))),
        (Some(p), _) => Some((p, None)),
        (None, Some((id, r))) => Some((r, Some(id))),
        (None, None) => None,
    })
}

/// Opens a hosted checkout page for `plan_code` / `interval`.
///
/// A running in-app trial carries over (the first charge waits for its end)
/// unless a discount applies, which is spent on an immediate first charge.
pub async fn start_checkout(
    state: &AppState,
    user_id: Uuid,
    payload: &CheckoutPayload,
) -> Result<RedirectResponse, ApiError> {
    payload.validate()?;
    let payments = payments(state)?;
    ensure_enforced(state).await?;
    let plan = purchasable_plan(state, &payload.plan_code, payload.interval).await?;
    let account = load_account(state, user_id).await?;
    if account.email.is_none() || account.email_verified_at.is_none() {
        return Err(ApiError::rule(
            StatusCode::FORBIDDEN,
            codes::EMAIL_NOT_VERIFIED,
            "Verify your e-mail address before subscribing: receipts are sent there.",
        ));
    }

    let timestamp = now();
    let subscription = state.billing_repo.get_subscription(user_id).await?;
    if subscription
        .as_ref()
        .is_some_and(|s| s.source == SubscriptionSource::Payment && s.is_effective(timestamp))
    {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::PAID_SUBSCRIPTION_ACTIVE,
            "You already have a paid subscription; change its plan instead.",
        ));
    }

    let locale = user_locale(state, user_id).await?;
    let discount = best_discount(state, user_id, &plan.code).await?;
    let trial_end = match (&discount, &subscription) {
        (None, Some(s))
            if s.status == SubscriptionStatus::Trialing && s.is_effective(timestamp) =>
        {
            s.trial_ends_at
                .filter(|end| *end > timestamp + Duration::hours(MIN_TRIAL_HOURS))
                .map(|end| end.and_utc().timestamp())
        }
        _ => None,
    };

    let customer_id = ensure_customer(state, payments, user_id, &account, &locale).await?;
    let spec = price_spec(&plan, payload.interval);
    let price_id = payments.gateway.ensure_price(&spec).await?;
    let (coupon_id, redemption_id) = match discount {
        Some((percent, redemption)) => (
            Some(
                payments
                    .gateway
                    .create_coupon(percent, &format!("{} -{percent}%", spec.product_name))
                    .await?,
            ),
            redemption,
        ),
        None => (None, None),
    };

    let url = payments
        .gateway
        .create_checkout(&CheckoutRequest {
            user_id,
            customer_id,
            price_id,
            plan_code: plan.code.clone(),
            interval: payload.interval,
            success_url: app_url(&locale, "/dashboard/settings?checkout=success#subscription"),
            cancel_url: app_url(
                &locale,
                "/dashboard/settings?checkout=canceled#subscription",
            ),
            locale,
            trial_end,
            coupon_id,
            redemption_id,
        })
        .await?;
    info!(%user_id, plan = %plan.code, interval = payload.interval.key(), "Checkout started");
    Ok(RedirectResponse { url })
}

/// The account's paid subscription id, while it is in effect.
async fn paid_subscription_ref(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let row: Option<(String, SubscriptionStatus, Option<NaiveDateTime>)> = sqlx::query_as(
        "SELECT external_ref, status, current_period_end FROM subscriptions
         WHERE user_id = $1 AND source = 'payment' AND external_ref IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row
        .filter(|(_, status, end)| {
            crate::models::billing::subscription_in_effect(
                *status,
                SubscriptionSource::Payment,
                *end,
                now(),
            )
        })
        .map(|(id, _, _)| id))
}

fn no_paid_subscription() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::NO_PAID_SUBSCRIPTION,
        "This account has no paid subscription.",
    )
}

/// Moves the paid subscription to another plan or interval. The prorated
/// difference is charged right away and the change only applies if that
/// charge succeeds.
pub async fn change_plan(
    state: &AppState,
    user_id: Uuid,
    payload: &CheckoutPayload,
) -> Result<BillingMe, ApiError> {
    payload.validate()?;
    let payments = payments(state)?;
    ensure_enforced(state).await?;
    let plan = purchasable_plan(state, &payload.plan_code, payload.interval).await?;
    let Some(subscription_id) = paid_subscription_ref(state, user_id).await? else {
        return Err(no_paid_subscription());
    };

    let current = payments.gateway.get_subscription(&subscription_id).await?;
    match current.status.as_str() {
        "active" | "trialing" => {}
        "past_due" | "unpaid" => {
            return Err(ApiError::rule(
                StatusCode::CONFLICT,
                codes::SUBSCRIPTION_PAST_DUE,
                "Your last payment failed. Update your payment method before changing plans.",
            ));
        }
        _ => return Err(no_paid_subscription()),
    }
    if current.cancel_at_period_end {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::SUBSCRIPTION_CANCELING,
            "Your subscription is set to end. Renew it in the billing portal before changing plans.",
        ));
    }

    let price_id = payments
        .gateway
        .ensure_price(&price_spec(&plan, payload.interval))
        .await?;
    if price_id == current.price_id
        || (current.plan_code.as_deref() == Some(plan.code.as_str())
            && current.interval == Some(payload.interval))
    {
        return Err(ApiError::rule(
            StatusCode::CONFLICT,
            codes::PLAN_ALREADY_ACTIVE,
            "You are already on this plan.",
        ));
    }

    let updated = payments
        .gateway
        .change_price(&current.id, &current.item_id, &price_id)
        .await?;
    if updated.has_pending_update {
        return Err(ApiError::rule(
            StatusCode::PAYMENT_REQUIRED,
            codes::PAYMENT_DECLINED,
            "The payment for the new plan was not approved. Your plan is unchanged.",
        ));
    }
    // Applied now rather than when the webhook arrives, so the answer
    // already shows the new plan.
    mirror(state, user_id, &updated, None, None).await?;
    info!(%user_id, plan = %plan.code, interval = payload.interval.key(), "Plan changed");
    billing::billing_me(state, user_id).await
}

/// Opens the hosted billing portal (card, invoices, cancellation).
pub async fn open_portal(state: &AppState, user_id: Uuid) -> Result<RedirectResponse, ApiError> {
    let payments = payments(state)?;
    let account = load_account(state, user_id).await?;
    let Some(customer_id) = account.stripe_customer_id else {
        return Err(no_paid_subscription());
    };
    let locale = user_locale(state, user_id).await?;
    let url = payments
        .gateway
        .create_portal(
            &customer_id,
            &app_url(&locale, "/dashboard/settings#subscription"),
            &locale,
        )
        .await?;
    Ok(RedirectResponse { url })
}

/// Ends the account's paid subscription at the provider right away (the
/// account is being deleted or staff revoked its plan). Without a paid
/// subscription this does nothing. When payments aren't configured the
/// provider can't be reached: logged, and the caller goes ahead.
pub async fn cancel_paid_subscription(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let Some(subscription_id) = paid_subscription_ref(state, user_id).await? else {
        return Ok(());
    };
    match &state.payments {
        Some(payments) => {
            payments.gateway.cancel_now(&subscription_id).await?;
            info!(%user_id, subscription = %subscription_id, "Paid subscription canceled");
        }
        None => warn!(
            %user_id,
            subscription = %subscription_id,
            "Payments are not configured: cancel this subscription in the provider's dashboard"
        ),
    }
    Ok(())
}

fn snapshot_of(subscription: &GatewaySubscription) -> Option<PaymentSnapshot> {
    Some(PaymentSnapshot {
        external_ref: subscription.id.clone(),
        plan_code: subscription.plan_code.clone()?,
        interval: subscription.interval,
        status: map_provider_status(&subscription.status),
        provider_status: subscription.status.clone(),
        current_period_end: subscription.current_period_end.and_then(unix_to_naive),
        cancel_at_period_end: subscription.cancel_at_period_end,
        ended_at: subscription.ended_at.and_then(unix_to_naive),
    })
}

/// Applies `subscription` to `user_id`. With `event`, only once per event
/// id (a redelivery is a no-op). `redemption_id` is the promo code spent
/// on the purchase.
async fn mirror(
    state: &AppState,
    user_id: Uuid,
    subscription: &GatewaySubscription,
    event: Option<(&str, &str)>,
    redemption_id: Option<Uuid>,
) -> Result<(), ApiError> {
    let Some(snapshot) = snapshot_of(subscription) else {
        warn!(
            subscription = %subscription.id,
            "Subscription without a Setlyst plan (created outside the app?); ignored"
        );
        return Ok(());
    };

    let mut tx = state.db.begin().await?;
    if let Some((id, kind)) = event {
        let first = sqlx::query(
            "INSERT INTO stripe_events (id, type, received_at) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(kind)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        if first.rows_affected() == 0 {
            return Ok(());
        }
    }
    sqlx::query(
        "UPDATE users SET stripe_customer_id = $2
         WHERE id = $1 AND stripe_customer_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM users WHERE stripe_customer_id = $2)",
    )
    .bind(user_id)
    .bind(&subscription.customer_id)
    .execute(&mut *tx)
    .await?;
    let change = apply_payment_subscription(&mut tx, user_id, &snapshot).await?;
    if let Some(redemption_id) = redemption_id {
        sqlx::query(
            "UPDATE promo_redemptions SET applied_at = $3
             WHERE id = $1 AND user_id = $2 AND applied_at IS NULL",
        )
        .bind(redemption_id)
        .bind(user_id)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    if let Some(change) = change {
        info!(%user_id, kind = change.kind, plan = %change.plan_code, "Paid subscription updated");
        let notifications: Vec<Notification> = change.notification(user_id).into_iter().collect();
        notify_all(state, notifications).await;
    }
    Ok(())
}

async fn user_exists(state: &AppState, user_id: Uuid) -> Result<bool, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
            .bind(user_id)
            .fetch_one(&state.db)
            .await?,
    )
}

/// The account a subscription belongs to: its checkout metadata, else its
/// customer, else the checkout's reference.
async fn owner_of(
    state: &AppState,
    subscription: &GatewaySubscription,
    client_reference: Option<&str>,
) -> Result<Option<Uuid>, ApiError> {
    if let Some(id) = subscription.user_id
        && user_exists(state, id).await?
    {
        return Ok(Some(id));
    }
    if !subscription.customer_id.is_empty() {
        let by_customer: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE stripe_customer_id = $1")
                .bind(&subscription.customer_id)
                .fetch_optional(&state.db)
                .await?;
        if by_customer.is_some() {
            return Ok(by_customer);
        }
    }
    if let Some(id) = client_reference.and_then(|r| Uuid::parse_str(r).ok())
        && user_exists(state, id).await?
    {
        return Ok(Some(id));
    }
    Ok(None)
}

fn bad_webhook(message: &str) -> ApiError {
    ApiError::BadRequest(message.to_string())
}

/// Handles one webhook delivery: `body` is the raw request body and
/// `signature` its `Stripe-Signature` header.
///
/// Answers an error (so the provider retries) only when the event could not
/// be applied for a reason that may go away: the provider unreachable or a
/// database failure.
pub async fn handle_webhook(
    state: &AppState,
    body: &[u8],
    signature: Option<&str>,
) -> Result<(), ApiError> {
    let payments = payments(state)?;
    let signature = signature.ok_or_else(|| bad_webhook("Missing Stripe-Signature header."))?;
    webhook::verify_signature(
        body,
        signature,
        &payments.webhook_secret,
        Utc::now().timestamp(),
    )
    .map_err(|e| {
        warn!(error = ?e, "Rejected a webhook with an invalid signature");
        bad_webhook(match e {
            SignatureError::Expired => "Signature timestamp outside the tolerance.",
            _ => "Invalid signature.",
        })
    })?;

    let value: Value =
        serde_json::from_slice(body).map_err(|_| bad_webhook("The body is not JSON."))?;
    let event = webhook::parse_event(&value).ok_or_else(|| bad_webhook("Not an event."))?;
    if !HANDLED_EVENTS.contains(&event.kind.as_str()) {
        return Ok(());
    }
    let Some(subscription_id) = event.subscription_id.as_deref() else {
        return Ok(());
    };

    let subscription = payments.gateway.get_subscription(subscription_id).await?;
    let Some(user_id) =
        owner_of(state, &subscription, event.client_reference_id.as_deref()).await?
    else {
        warn!(
            event = %event.id,
            subscription = %subscription.id,
            "Webhook for a subscription of no known account; ignored"
        );
        return Ok(());
    };
    let redemption_id = event
        .redemption_id
        .as_deref()
        .filter(|_| event.kind.starts_with("checkout.session."))
        .and_then(|r| Uuid::parse_str(r).ok());
    mirror(
        state,
        user_id,
        &subscription,
        Some((&event.id, &event.kind)),
        redemption_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::quota::QuotaLimits;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn plan(monthly: i32, yearly: i32) -> Plan {
        Plan {
            code: "pro".into(),
            name: json!({"en": "Pro", "pt-BR": "Pro BR"}),
            description: json!({}),
            price_monthly_cents: monthly,
            price_yearly_cents: yearly,
            currency: "BRL".into(),
            limits: QuotaLimits::default(),
            features: BTreeMap::new(),
            highlighted: false,
            is_public: true,
            sort_order: 0,
            updated_at: now(),
        }
    }

    #[test]
    fn price_specs_come_from_the_plan_row() {
        let spec = price_spec(&plan(3990, 39900), BillingInterval::Yearly);
        assert_eq!(spec.unit_amount, 39900);
        assert_eq!(spec.product_name, "Setlyst Pro BR");
        assert_eq!(spec.currency, "BRL");
        assert_eq!(
            price_spec(&plan(3990, 39900), BillingInterval::Monthly).unit_amount,
            3990
        );
    }

    #[test]
    fn provider_statuses_map_to_local_ones() {
        assert_eq!(
            map_provider_status("trialing"),
            Some(SubscriptionStatus::Active)
        );
        assert_eq!(
            map_provider_status("active"),
            Some(SubscriptionStatus::Active)
        );
        assert_eq!(
            map_provider_status("past_due"),
            Some(SubscriptionStatus::PastDue)
        );
        assert_eq!(
            map_provider_status("canceled"),
            Some(SubscriptionStatus::Canceled)
        );
        assert_eq!(
            map_provider_status("unpaid"),
            Some(SubscriptionStatus::Expired)
        );
        assert_eq!(map_provider_status("incomplete"), None);
        assert_eq!(map_provider_status("incomplete_expired"), None);
    }

    #[test]
    fn snapshots_need_a_plan() {
        let subscription = GatewaySubscription {
            id: "sub_1".into(),
            customer_id: "cus_1".into(),
            status: "active".into(),
            item_id: "si_1".into(),
            price_id: "price_1".into(),
            plan_code: None,
            interval: Some(BillingInterval::Monthly),
            current_period_end: Some(1_800_000_000),
            cancel_at_period_end: false,
            canceled_at: None,
            ended_at: None,
            user_id: None,
            has_pending_update: false,
        };
        assert!(snapshot_of(&subscription).is_none());
        let snapshot = snapshot_of(&GatewaySubscription {
            plan_code: Some("pro".into()),
            ..subscription
        })
        .unwrap();
        assert_eq!(snapshot.status, Some(SubscriptionStatus::Active));
        assert_eq!(
            snapshot.current_period_end.unwrap().and_utc().timestamp(),
            1_800_000_000
        );
    }
}
