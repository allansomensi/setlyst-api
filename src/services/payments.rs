//! Paid subscriptions: checkout, plan changes, the billing portal,
//! withdrawal and refunds, and the payment provider's webhook.
//!
//! The provider is the source of truth for paid subscriptions. Every
//! webhook event is only a hint: the subscription it names is fetched fresh
//! and mirrored locally (`billing::apply_payment_subscription`), so events
//! may arrive late, twice or out of order without consequence. Each event
//! id is applied at most once, in the same transaction as its change, and
//! every fetch-and-apply of one subscription holds a lock on that
//! subscription id, so two deliveries can't commit a stale snapshot over a
//! newer one.
//!
//! An account has at most one paid subscription: checkout refuses while the
//! provider has a live one for the customer, a new checkout expires the
//! pages still open, and a second subscription that gets through anyway is
//! canceled and refunded instead of replacing the first.

use crate::{
    config::Config,
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::{ApiError, codes},
    models::{
        billing::{
            BillingInterval, BillingMe, CheckoutPayload, PAYMENT_GRACE_DAYS, Plan,
            RENEWAL_GRACE_DAYS, RedirectResponse, SubscriptionSource, SubscriptionStatus,
            WITHDRAWAL_DAYS, WithdrawResponse,
        },
        user::CURRENT_TERMS_VERSION,
    },
    payments::{
        CheckoutRequest, GatewayInvoice, GatewaySubscription, PaymentError, Payments, PriceSpec,
        webhook::{self, SignatureError, StripeEvent},
    },
    services::{
        account::user_locale,
        billing::{
            self, Applied, PaymentChange, PaymentSnapshot, apply_payment_subscription,
            end_paid_subscription, map_provider_status, record_event_row, revoke_shares_if_lost,
        },
        finance,
        notifier::notify_all,
    },
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use tracing::{error, info, warn};
use uuid::Uuid;
use validator::Validate;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// The provider refuses a trial ending sooner than this (Stripe: 48 h).
const MIN_TRIAL_HOURS: i64 = 49;

/// The provider refuses a trial ending later than 730 days out.
const MAX_TRIAL_DAYS: i64 = 729;

/// Stripe expires a Checkout Session after 24 hours.
const CHECKOUT_SESSION_HOURS: i64 = 24;

/// Safety limit for one reconciliation walk (100 subscriptions per page).
const MAX_RECONCILE_PAGES: usize = 50;

/// Audit actions of refunds with cancellation.
pub const AUDIT_WITHDRAWN: &str = "billing.subscription_withdrawn";
pub const AUDIT_REFUNDED: &str = "billing.subscription_refunded";

/// Webhook event types that can change a subscription (it is fetched and
/// mirrored).
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
    "customer.subscription.trial_will_end",
    "invoice.paid",
    "invoice.payment_failed",
    "invoice.payment_action_required",
    "invoice.upcoming",
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

/// The web app's public origin.
fn app_base() -> String {
    Config::try_get()
        .map(|c| c.app_base_url.clone())
        .unwrap_or_else(|| "http://localhost:3000".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// An absolute link into the web app.
fn app_url(locale: &str, path: &str) -> String {
    format!("{}/{locale}{path}", app_base())
}

/// Namespace of the per-subscription advisory lock (see [`lock_provider_subscription`]).
const SUBSCRIPTION_LOCK_PREFIX: &str = "stripe-sub:";
/// Namespace of the per-account checkout lock.
const CHECKOUT_LOCK_PREFIX: &str = "checkout:";

/// Serializes every fetch-and-apply of one provider subscription until the
/// transaction ends. Taken before the subscription is fetched, so the
/// snapshot applied is never older than one already committed.
async fn lock_provider_subscription(
    conn: &mut PgConnection,
    subscription_id: &str,
) -> Result<(), ApiError> {
    advisory_lock(
        conn,
        &format!("{SUBSCRIPTION_LOCK_PREFIX}{subscription_id}"),
    )
    .await
}

async fn advisory_lock(conn: &mut PgConnection, key: &str) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[derive(Debug, FromRow)]
struct Account {
    username: String,
    email: Option<String>,
    email_verified_at: Option<NaiveDateTime>,
    stripe_customer_id: Option<String>,
    stripe_customer_generation: i32,
}

async fn load_account(state: &AppState, user_id: Uuid) -> Result<Account, ApiError> {
    sqlx::query_as(
        "SELECT username, email, email_verified_at, stripe_customer_id, stripe_customer_generation
         FROM users WHERE id = $1",
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

/// Forgets a customer that no longer exists at the provider (deleted in
/// the dashboard): the next checkout creates a new one, under a new
/// idempotency key.
async fn clear_customer(state: &AppState, customer_id: &str) -> Result<u64, ApiError> {
    let cleared = sqlx::query(
        "UPDATE users SET stripe_customer_id = NULL,
                          stripe_customer_generation = stripe_customer_generation + 1
         WHERE stripe_customer_id = $1",
    )
    .bind(customer_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if cleared > 0 {
        warn!(customer = %customer_id, "Stripe customer no longer exists; forgotten");
    }
    Ok(cleared)
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
        .create_customer(
            user_id,
            account.email.as_deref(),
            &account.username,
            locale,
            account.stripe_customer_generation,
        )
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
/// win ties, keeping the code for later. A code disabled or expired since
/// it was redeemed no longer counts.
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
           AND p.discount_percent IS NOT NULL AND p.disabled_at IS NULL
           AND (p.expires_at IS NULL OR p.expires_at > $2)
         ORDER BY p.discount_percent DESC, r.redeemed_at
         LIMIT 1",
    )
    .bind(user_id)
    .bind(now())
    .fetch_optional(&state.db)
    .await?;
    Ok(match (promotion, redemption) {
        (Some(p), Some((id, r))) if r > p => Some((r, Some(id))),
        (Some(p), _) => Some((p, None)),
        (None, Some((id, r))) => Some((r, Some(id))),
        (None, None) => None,
    })
}

/// Text next to the required "I accept the Terms of Service" box on the
/// provider's checkout page: links the Subscription Terms and states the
/// automatic renewal, free cancellation and the 7-day withdrawal right
/// (Decreto 7.962 art. 4, I; CDC art. 46 and 49). Markdown links are
/// rendered by Stripe.
pub fn terms_message(locale: &str) -> String {
    let url = app_url(locale, "/legal/subscription");
    let text = match locale {
        "pt-BR" => {
            "Li e aceito os [Termos de Assinatura]({url}). A assinatura renova automaticamente pelo preço vigente; você pode cancelar quando quiser e desistir em até 7 dias da cobrança com reembolso integral."
        }
        "es" => {
            "Leí y acepto los [Términos de Suscripción]({url}). La suscripción se renueva automáticamente al precio vigente; puedes cancelarla cuando quieras y desistir hasta 7 días después del cobro con reembolso total."
        }
        _ => {
            "I have read and accept the [Subscription Terms]({url}). The subscription renews automatically at the current price; you can cancel at any time and withdraw within 7 days of the charge for a full refund."
        }
    };
    text.replace("{url}", &url)
}

fn paid_subscription_active() -> ApiError {
    ApiError::rule(
        StatusCode::CONFLICT,
        codes::PAID_SUBSCRIPTION_ACTIVE,
        "You already have a paid subscription; change its plan instead.",
    )
}

/// `true` when the provider has a subscription of `customer_id` that
/// charges (or is about to). `None` when the customer no longer exists.
async fn provider_has_live_subscription(
    payments: &Payments,
    customer_id: &str,
) -> Result<Option<bool>, ApiError> {
    match payments
        .gateway
        .list_subscriptions(Some(customer_id), None)
        .await
    {
        Ok(page) => Ok(Some(page.subscriptions.iter().any(|s| s.may_charge()))),
        Err(e) if e.is_missing() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Opens a hosted checkout page for `plan_code` / `interval`.
///
/// A running in-app trial carries over (the first charge waits for its
/// end, at most 729 days out); a discount applies to that first charge.
/// Refused while the account (or its customer at the provider) has a paid
/// subscription; pages of earlier attempts still open are expired first.
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
        return Err(paid_subscription_active());
    }

    let locale = user_locale(state, user_id).await?;
    let discount = best_discount(state, user_id, &plan.code).await?;
    let trial_end = subscription
        .as_ref()
        .filter(|s| s.status == SubscriptionStatus::Trialing && s.is_effective(timestamp))
        .and_then(|s| s.trial_ends_at)
        .filter(|end| *end > timestamp + Duration::hours(MIN_TRIAL_HOURS))
        .map(|end| end.min(timestamp + Duration::days(MAX_TRIAL_DAYS)))
        .map(|end| end.and_utc().timestamp());

    // The provider's view: a subscription paid in another tab may not have
    // reached the webhook yet.
    let mut customer_id = ensure_customer(state, payments, user_id, &account, &locale).await?;
    match provider_has_live_subscription(payments, &customer_id).await? {
        Some(true) => return Err(paid_subscription_active()),
        Some(false) => {}
        None => {
            clear_customer(state, &customer_id).await?;
            let account = load_account(state, user_id).await?;
            customer_id = ensure_customer(state, payments, user_id, &account, &locale).await?;
        }
    }

    let spec = price_spec(&plan, payload.interval);
    let price_id = payments.gateway.ensure_price(&spec).await?;
    let (coupon_id, redemption_id) = match discount {
        Some((percent, redemption)) => {
            let key = format!(
                "setlyst-coupon-{user_id}-{percent}-{}-{}",
                redemption.map_or_else(|| "promotion".to_string(), |r| r.to_string()),
                timestamp.and_utc().timestamp() / 3600
            );
            (
                Some(
                    payments
                        .gateway
                        .create_coupon(percent, &format!("{} -{percent}%", spec.product_name), &key)
                        .await?,
                ),
                redemption,
            )
        }
        None => (None, None),
    };

    // One checkout at a time per account: earlier pages still open are
    // expired before a new one is opened, so they can't be paid later.
    let mut tx = state.db.begin().await?;
    advisory_lock(&mut tx, &format!("{CHECKOUT_LOCK_PREFIX}{user_id}")).await?;
    let open: Vec<(String, NaiveDateTime)> = sqlx::query_as(
        "SELECT session_id, created_at FROM checkout_sessions
         WHERE user_id = $1 AND expired_at IS NULL AND completed_at IS NULL",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;
    for (session_id, created_at) in &open {
        if *created_at > timestamp - Duration::hours(CHECKOUT_SESSION_HOURS) {
            payments.gateway.expire_checkout(session_id).await?;
        }
        sqlx::query("UPDATE checkout_sessions SET expired_at = $2 WHERE session_id = $1")
            .bind(session_id)
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
    }
    let attempt: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM checkout_sessions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;

    let session = payments
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
            terms_message: Some(terms_message(&locale)),
            terms_version: Some(CURRENT_TERMS_VERSION.to_string()),
            idempotency_key: format!(
                "setlyst-checkout-{user_id}-{}-{}-{attempt}",
                plan.code,
                payload.interval.key()
            ),
            locale,
            trial_end,
            coupon_id,
            redemption_id,
        })
        .await?;
    sqlx::query(
        "INSERT INTO checkout_sessions (session_id, user_id, created_at) VALUES ($1, $2, $3)
         ON CONFLICT (session_id) DO NOTHING",
    )
    .bind(&session.id)
    .bind(user_id)
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(%user_id, plan = %plan.code, interval = payload.interval.key(), "Checkout started");
    Ok(RedirectResponse { url: session.url })
}

/// The account's paid subscription id, while it is in effect.
async fn paid_subscription_ref(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let row: Option<(
        String,
        SubscriptionStatus,
        Option<NaiveDateTime>,
        Option<NaiveDateTime>,
    )> = sqlx::query_as(
        "SELECT external_ref, status, current_period_end, past_due_since FROM subscriptions
         WHERE user_id = $1 AND source = 'payment' AND external_ref IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row
        .filter(|(_, status, end, past_due_since)| {
            crate::models::billing::subscription_in_effect(
                *status,
                SubscriptionSource::Payment,
                *end,
                *past_due_since,
                now(),
            )
        })
        .map(|(id, ..)| id))
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
/// charge succeeds; a charge that needs the customer's authentication
/// (3-D Secure) answers `PAYMENT_ACTION_REQUIRED` with the page to do it.
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

    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, &subscription_id).await?;
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
        return Err(pending_change_error(&updated));
    }
    // Applied now rather than when the webhook arrives, so the answer
    // already shows the new plan.
    let outcome = mirror_in(state, &mut tx, user_id, &updated, &MirrorOptions::default()).await?;
    tx.commit().await?;
    after_change(state, user_id, outcome.change).await;
    info!(%user_id, plan = %plan.code, interval = payload.interval.key(), "Plan changed");
    billing::billing_me(state, user_id).await
}

/// Why a plan change is still pending: the customer must authenticate the
/// charge on the provider's invoice page, or the card was declined.
fn pending_change_error(updated: &GatewaySubscription) -> ApiError {
    let declined = updated.latest_payment_status.as_deref() == Some("requires_payment_method");
    match updated
        .hosted_invoice_url
        .as_deref()
        .filter(|_| updated.latest_invoice_status.as_deref() == Some("open") && !declined)
    {
        Some(url) => ApiError::rule_with_meta(
            StatusCode::PAYMENT_REQUIRED,
            codes::PAYMENT_ACTION_REQUIRED,
            "Your bank needs you to confirm this payment. The new plan applies once it's confirmed.",
            json!({ "hosted_invoice_url": url }),
        ),
        None => ApiError::rule(
            StatusCode::PAYMENT_REQUIRED,
            codes::PAYMENT_DECLINED,
            "The payment for the new plan was not approved. Your plan is unchanged.",
        ),
    }
}

/// Opens the hosted billing portal (card, invoices, cancellation).
pub async fn open_portal(state: &AppState, user_id: Uuid) -> Result<RedirectResponse, ApiError> {
    let payments = payments(state)?;
    let account = load_account(state, user_id).await?;
    let Some(customer_id) = account.stripe_customer_id else {
        return Err(no_paid_subscription());
    };
    let locale = user_locale(state, user_id).await?;
    match payments
        .gateway
        .create_portal(
            &customer_id,
            &app_url(&locale, "/dashboard/settings#subscription"),
            &locale,
        )
        .await
    {
        Ok(url) => Ok(RedirectResponse { url }),
        // Deleted at the provider: there is nothing to manage any more.
        Err(e) if e.is_missing() => {
            clear_customer(state, &customer_id).await?;
            Err(no_paid_subscription())
        }
        Err(e) => Err(e.into()),
    }
}

/// Ends every subscription of the account at the provider right away (the
/// account is being deleted or staff revoked its plan): the mirrored one
/// and any other the customer has that may still charge. Without a paid
/// subscription this does nothing. When payments aren't configured the
/// provider can't be reached: logged, and the caller goes ahead.
pub async fn cancel_paid_subscription(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let mirrored = paid_subscription_ref(state, user_id).await?;
    let customer = customer_of(state, user_id).await?;
    let Some(payments) = &state.payments else {
        if let Some(subscription_id) = mirrored {
            warn!(
                %user_id,
                subscription = %subscription_id,
                "Payments are not configured: cancel this subscription in the provider's dashboard"
            );
        }
        return Ok(());
    };
    let mut ids: Vec<String> = mirrored.into_iter().collect();
    if let Some(customer) = &customer {
        match payments
            .gateway
            .list_subscriptions(Some(customer), None)
            .await
        {
            Ok(page) => ids.extend(
                page.subscriptions
                    .into_iter()
                    .filter(|s| s.may_charge())
                    .map(|s| s.id),
            ),
            Err(e) if e.is_missing() => {}
            Err(e) => return Err(e.into()),
        }
    }
    ids.sort();
    ids.dedup();
    for subscription_id in ids {
        payments.gateway.cancel_now(&subscription_id).await?;
        info!(%user_id, subscription = %subscription_id, "Paid subscription canceled");
    }
    Ok(())
}

/// The account's customer at the payment provider, if it ever had one.
pub async fn customer_of(state: &AppState, user_id: Uuid) -> Result<Option<String>, ApiError> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT stripe_customer_id FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .flatten())
}

/// Deletes the provider-side customer of a deleted account (card details
/// and contact data held there; LGPD art. 16). The account is already
/// gone, so a failure is not returned: the customer is queued and the
/// hourly job retries until the provider confirms.
pub async fn forget_customer(state: &AppState, customer_id: Option<String>) {
    let Some(customer_id) = customer_id else {
        return;
    };
    let error = match &state.payments {
        Some(payments) => match payments.gateway.delete_customer(&customer_id).await {
            Ok(()) => {
                info!(customer = %customer_id, "Payment provider customer deleted");
                return;
            }
            Err(e) => e.to_string(),
        },
        None => "payments are not configured".to_string(),
    };
    warn!(customer = %customer_id, error = %error, "Could not delete the payment provider customer; queued for retry");
    let queued = sqlx::query(
        "INSERT INTO stripe_cleanup_queue (customer_id, attempts, last_error, next_attempt_at, created_at)
         VALUES ($1, 1, $2, $3, $4)
         ON CONFLICT (customer_id) DO NOTHING",
    )
    .bind(&customer_id)
    .bind(error.chars().take(500).collect::<String>())
    .bind(now() + Duration::hours(1))
    .bind(now())
    .execute(&state.db)
    .await;
    if let Err(e) = queued {
        error!(customer = %customer_id, error = %e, "Could not queue the customer deletion; delete it in the provider's dashboard");
    }
}

/// Retries the queued customer deletions that are due. Returns how many
/// were deleted.
pub async fn process_cleanup_queue(state: &AppState) -> Result<u64, ApiError> {
    let Some(payments) = &state.payments else {
        return Ok(0);
    };
    let due: Vec<(String, i32)> = sqlx::query_as(
        "SELECT customer_id, attempts FROM stripe_cleanup_queue
         WHERE next_attempt_at <= $1 ORDER BY next_attempt_at LIMIT 50",
    )
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    let mut deleted = 0;
    for (customer_id, attempts) in due {
        match payments.gateway.delete_customer(&customer_id).await {
            Ok(()) => {
                sqlx::query("DELETE FROM stripe_cleanup_queue WHERE customer_id = $1")
                    .bind(&customer_id)
                    .execute(&state.db)
                    .await?;
                info!(customer = %customer_id, "Queued provider customer deleted");
                deleted += 1;
            }
            Err(e) => {
                // Hourly at first, then at most daily.
                let wait = Duration::hours(2_i64.pow(attempts.clamp(0, 5) as u32).min(24));
                sqlx::query(
                    "UPDATE stripe_cleanup_queue
                     SET attempts = attempts + 1, last_error = $2, next_attempt_at = $3
                     WHERE customer_id = $1",
                )
                .bind(&customer_id)
                .bind(e.to_string().chars().take(500).collect::<String>())
                .bind(now() + wait)
                .execute(&state.db)
                .await?;
                if attempts >= 5 {
                    error!(customer = %customer_id, attempts, error = %e, "Provider customer still not deleted; delete it in the dashboard");
                }
            }
        }
    }
    Ok(deleted)
}

/// Copies the account's e-mail and username to its provider customer, so
/// receipts, dunning and 3-D Secure links go to the current address. Call
/// it after the account's e-mail changes. Best effort: a provider failure
/// is logged, not returned.
pub async fn sync_customer_email(state: &AppState, user_id: Uuid) -> Result<(), ApiError> {
    let Some(payments) = &state.payments else {
        return Ok(());
    };
    let account = load_account(state, user_id).await?;
    let Some(customer_id) = account.stripe_customer_id else {
        return Ok(());
    };
    match payments
        .gateway
        .update_customer(&customer_id, account.email.as_deref(), &account.username)
        .await
    {
        Ok(()) => info!(%user_id, "Payment provider customer e-mail updated"),
        Err(e) if e.is_missing() => {
            clear_customer(state, &customer_id).await?;
        }
        Err(e) => {
            warn!(%user_id, error = %e, "Could not update the payment provider customer e-mail")
        }
    }
    Ok(())
}

/// What [`probe_permissions`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PermissionProbe {
    /// Scopes the key is refused (HTTP 403): a restricted key without them.
    pub denied: Vec<String>,
    /// Scopes that couldn't be checked (network, invalid key, 5xx).
    pub unchecked: Vec<String>,
}

impl PermissionProbe {
    pub fn is_clean(&self) -> bool {
        self.denied.is_empty() && self.unchecked.is_empty()
    }
}

/// Checks at startup that the API key can read what the webhook and the
/// finance report need (a restricted key missing a scope makes every
/// `invoice.paid` fail). Logs an error per failed scope. Only `denied`
/// scopes are certainly missing; `unchecked` ones failed for another
/// reason (Stripe unreachable, revoked key).
pub async fn probe_permissions(state: &AppState) -> PermissionProbe {
    let mut probe = PermissionProbe::default();
    let Some(payments) = &state.payments else {
        return probe;
    };
    for (scope, e) in payments.gateway.probe_permissions().await {
        if e.is_permission_denied() {
            error!(scope = %scope, error = %e, "The Stripe API key lacks a permission the API needs");
            probe.denied.push(scope);
        } else {
            error!(scope = %scope, error = %e, "Could not check a Stripe API key permission");
            probe.unchecked.push(scope);
        }
    }
    probe
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
        trial_end: subscription.trial_end.and_then(unix_to_naive),
        unit_amount: subscription.unit_amount,
        currency: subscription.currency.clone(),
    })
}

/// Extra effects of one mirror.
#[derive(Debug, Default)]
struct MirrorOptions<'a> {
    /// `(id, type)` of the webhook event: applied at most once.
    event: Option<(&'a str, &'a str)>,
    /// The promo code spent on the purchase.
    redemption_id: Option<Uuid>,
    /// The Subscription Terms version accepted at checkout.
    terms_version: Option<String>,
}

/// What one mirror changed (`None` also for a webhook event already
/// applied).
#[derive(Debug, Default)]
struct MirrorOutcome {
    change: Option<PaymentChange>,
}

/// Applies `subscription` to `user_id` in `tx`. The caller holds the lock
/// of that subscription ([`lock_provider_subscription`]) and commits.
async fn mirror_in(
    state: &AppState,
    tx: &mut PgConnection,
    user_id: Uuid,
    subscription: &GatewaySubscription,
    options: &MirrorOptions<'_>,
) -> Result<MirrorOutcome, ApiError> {
    let Some(snapshot) = snapshot_of(subscription) else {
        warn!(
            subscription = %subscription.id,
            "Subscription without a Setlyst plan (created outside the app?); ignored"
        );
        return Ok(MirrorOutcome::default());
    };

    if let Some((id, kind)) = options.event {
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
            return Ok(MirrorOutcome::default());
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

    let change = match apply_payment_subscription(tx, user_id, &snapshot).await? {
        Applied::Changed(change) => Some(change),
        Applied::Unchanged => None,
        Applied::Duplicate { current_ref } => {
            resolve_duplicate(state, tx, user_id, subscription, &snapshot, &current_ref).await?
        }
    };

    if let Some(redemption_id) = options.redemption_id {
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
    if let Some(version) = &options.terms_version {
        sqlx::query(
            "UPDATE subscriptions SET terms_version = $3, terms_accepted_at = COALESCE(terms_accepted_at, $4)
             WHERE user_id = $1 AND source = 'payment' AND external_ref = $2",
        )
        .bind(user_id)
        .bind(&subscription.id)
        .bind(version.chars().take(40).collect::<String>())
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    Ok(MirrorOutcome { change })
}

/// A second live subscription of an account that already has one in
/// effect (`current_ref`). While the current one really is live at the
/// provider, the new one is canceled and its first charge refunded: the
/// buyer is never charged twice. When the current one turns out to be
/// over, it is ended here and the new one takes its place.
async fn resolve_duplicate(
    state: &AppState,
    tx: &mut PgConnection,
    user_id: Uuid,
    duplicate: &GatewaySubscription,
    snapshot: &PaymentSnapshot,
    current_ref: &str,
) -> Result<Option<PaymentChange>, ApiError> {
    let payments = payments(state)?;
    let current = match payments.gateway.get_subscription(current_ref).await {
        Ok(current) => Some(current),
        Err(e) if e.is_missing() => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(current) = current
        .as_ref()
        .filter(|c| matches!(c.status.as_str(), "active" | "trialing" | "past_due"))
    {
        error!(
            %user_id,
            kept = %current.id,
            duplicate = %duplicate.id,
            "Second paid subscription for one account: canceling and refunding it"
        );
        let refunded = cancel_duplicate(state, payments, duplicate).await?;
        record_event_row(
            tx,
            user_id,
            "duplicate_canceled",
            Some(&snapshot.plan_code),
            json!({
                "source": SubscriptionSource::Payment,
                "external_ref": duplicate.id,
                "kept": current.id,
                "refunded_cents": refunded,
            }),
            None,
        )
        .await?;
        return Ok(None);
    }

    // The current subscription is over at the provider: end it here, then
    // apply the new one.
    let ended = match current.as_ref().and_then(snapshot_of) {
        Some(current_snapshot) => {
            match apply_payment_subscription(tx, user_id, &current_snapshot).await? {
                Applied::Changed(change) => Some(change),
                _ => None,
            }
        }
        None => None,
    };
    if ended.is_none() {
        end_paid_subscription(
            tx,
            user_id,
            current_ref,
            "canceled",
            json!({ "reason": "replaced" }),
            None,
        )
        .await?;
    }
    match apply_payment_subscription(tx, user_id, snapshot).await? {
        Applied::Changed(change) => Ok(Some(change)),
        _ => Ok(None),
    }
}

/// Cancels a duplicate subscription now and refunds its latest paid
/// invoice. Returns the amount refunded.
async fn cancel_duplicate(
    state: &AppState,
    payments: &Payments,
    duplicate: &GatewaySubscription,
) -> Result<i64, ApiError> {
    payments.gateway.cancel_now(&duplicate.id).await?;
    let Some(invoice_id) = &duplicate.latest_invoice_id else {
        return Ok(0);
    };
    let Some(invoice) = payments.gateway.get_paid_invoice(invoice_id).await? else {
        return Ok(0);
    };
    let Some(payment_intent) = &invoice.payment_intent_id else {
        return Ok(0);
    };
    let refund = match payments
        .gateway
        .refund(
            payment_intent,
            None,
            &format!("setlyst-duplicate-{}", invoice.id),
        )
        .await
    {
        Ok(refund) => refund,
        Err(e) if already_refunded(&e) => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    if let Err(e) = finance::record_invoice(state, &invoice).await {
        error!(invoice = %invoice.id, error = %e, "Could not record the duplicate's invoice");
    }
    if let Err(e) = finance::upsert_refund(state, &refund).await {
        error!(refund = %refund.id, error = %e, "Could not record the duplicate's refund");
    }
    Ok(if refund.counts() { refund.amount } else { 0 })
}

fn already_refunded(e: &PaymentError) -> bool {
    e.code.as_deref() == Some("charge_already_refunded")
}

/// What follows a committed change: the owner's notice, and revoking
/// public links the account's plan no longer includes.
async fn after_change(state: &AppState, user_id: Uuid, change: Option<PaymentChange>) {
    let Some(change) = change else {
        return;
    };
    info!(%user_id, kind = change.kind, plan = %change.plan_code, "Paid subscription updated");
    notify_all(state, change.notification(user_id).into_iter().collect()).await;
    if change.ended()
        && let Err(e) = revoke_shares_if_lost(state, user_id).await
    {
        warn!(%user_id, error = %e, "Could not revoke public links");
    }
}

/// Fetches subscription `subscription_id` under its lock and mirrors it.
/// Returns the account it belongs to (`None`: nobody here).
async fn sync_subscription(
    state: &AppState,
    payments: &Payments,
    subscription_id: &str,
    client_reference: Option<&str>,
    options: &MirrorOptions<'_>,
) -> Result<Option<Uuid>, ApiError> {
    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, subscription_id).await?;
    let subscription = payments.gateway.get_subscription(subscription_id).await?;
    let Some(user_id) = owner_of(state, &subscription, client_reference).await? else {
        warn!(
            subscription = %subscription.id,
            "Webhook for a subscription of no known account; ignored"
        );
        return Ok(None);
    };
    let outcome = mirror_in(state, &mut tx, user_id, &subscription, options).await?;
    tx.commit().await?;
    after_change(state, user_id, outcome.change).await;
    Ok(Some(user_id))
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

    // A test-mode event on the live endpoint (or the reverse) must never
    // touch real subscriptions.
    if let Some(livemode) = event.livemode
        && livemode != payments.livemode
    {
        warn!(event = %event.id, kind = %event.kind, livemode, "Webhook event from the other Stripe mode; ignored");
        return Ok(());
    }
    let object = value.pointer("/data/object").unwrap_or(&Value::Null);

    match event.kind.as_str() {
        "charge.refunded" => return handle_charge_refunded(state, payments, object).await,
        "refund.created" | "refund.updated" | "charge.refund.updated" => {
            return finance::apply_refund_object(state, object).await;
        }
        "charge.dispute.created"
        | "charge.dispute.updated"
        | "charge.dispute.closed"
        | "charge.dispute.funds_withdrawn"
        | "charge.dispute.funds_reinstated" => {
            return handle_dispute(state, payments, &event, object).await;
        }
        "customer.deleted" => {
            if let Some(customer_id) = &event.object_id {
                clear_customer(state, customer_id).await?;
            }
            return Ok(());
        }
        "checkout.session.expired" => {
            if let Some(session_id) = &event.object_id {
                sqlx::query(
                    "UPDATE checkout_sessions SET expired_at = COALESCE(expired_at, $2)
                     WHERE session_id = $1",
                )
                .bind(session_id)
                .bind(now())
                .execute(&state.db)
                .await?;
            }
            return Ok(());
        }
        _ => {}
    }

    if !HANDLED_EVENTS.contains(&event.kind.as_str()) {
        return Ok(());
    }
    let checkout = event.kind.starts_with("checkout.session.");
    if event.kind == "checkout.session.completed"
        && let Some(session_id) = &event.object_id
    {
        sqlx::query(
            "UPDATE checkout_sessions SET completed_at = COALESCE(completed_at, $2)
             WHERE session_id = $1",
        )
        .bind(session_id)
        .bind(now())
        .execute(&state.db)
        .await?;
    }
    let options = MirrorOptions {
        event: Some((&event.id, &event.kind)),
        redemption_id: event
            .redemption_id
            .as_deref()
            .filter(|_| checkout)
            .and_then(|r| Uuid::parse_str(r).ok()),
        terms_version: (event.kind == "checkout.session.completed" && event.terms_accepted).then(
            || {
                event
                    .terms_version
                    .clone()
                    .unwrap_or_else(|| CURRENT_TERMS_VERSION.to_string())
            },
        ),
    };
    let user_id = match event.subscription_id.as_deref() {
        Some(subscription_id) => {
            sync_subscription(
                state,
                payments,
                subscription_id,
                event.client_reference_id.as_deref(),
                &options,
            )
            .await?
        }
        None => None,
    };

    // The finance ledger comes after the subscription and never fails the
    // event (Stripe would retry and eventually disable the endpoint); the
    // staff sync fills in what is missed.
    if event.kind == "invoice.paid"
        && let Some(invoice_id) = &event.object_id
        && let Err(e) = record_paid_invoice(state, payments, invoice_id).await
    {
        error!(invoice = %invoice_id, error = %e, "Could not record a paid invoice; run the finance sync");
    }
    let Some(user_id) = user_id else {
        return Ok(());
    };

    match event.kind.as_str() {
        "customer.subscription.trial_will_end" => {
            billing::send_paid_trial_reminders(state, Some(user_id)).await?;
        }
        "invoice.upcoming" => {
            let amount = object
                .get("amount_due")
                .and_then(Value::as_i64)
                .zip(object.get("currency").and_then(Value::as_str))
                .map(|(amount, currency)| (amount, currency.to_ascii_uppercase()));
            // Stripe announces renewals up to 30 days ahead (Dashboard
            // setting); only yearly plans are reminded.
            billing::send_renewal_reminders(state, Some(user_id), 45, amount).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Records a paid invoice in the finance ledger, with Stripe's fee when
/// known.
async fn record_paid_invoice(
    state: &AppState,
    payments: &Payments,
    invoice_id: &str,
) -> Result<(), ApiError> {
    let Some(invoice) = payments.gateway.get_paid_invoice(invoice_id).await? else {
        return Ok(());
    };
    finance::record_invoice(state, &invoice).await?;
    if let Some(payment_intent) = &invoice.payment_intent_id
        && let Some(fees) = payments.gateway.payment_fees(payment_intent).await?
    {
        finance::set_fees(state, &invoice.id, &fees).await?;
    }
    Ok(())
}

/// `charge.refunded`: the ledger first; then, when the whole latest
/// payment of the account's live subscription was refunded (a refund made
/// in the provider's dashboard), the subscription is canceled too, so it
/// isn't charged again next period.
async fn handle_charge_refunded(
    state: &AppState,
    payments: &Payments,
    charge: &Value,
) -> Result<(), ApiError> {
    let Some(payment_intent) = finance::apply_refund_event(state, charge).await? else {
        return Ok(());
    };
    let Some((user_id, subscription_id)) =
        finance::fully_refunded_latest_payment(state, &payment_intent).await?
    else {
        return Ok(());
    };
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM subscriptions
                        WHERE user_id = $1 AND source = 'payment' AND external_ref = $2
                          AND status IN ('active', 'past_due'))",
    )
    .bind(user_id)
    .bind(&subscription_id)
    .fetch_one(&state.db)
    .await?;
    if !live {
        return Ok(());
    }
    payments.gateway.cancel_now(&subscription_id).await?;
    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, &subscription_id).await?;
    let change = end_paid_subscription(
        &mut tx,
        user_id,
        &subscription_id,
        "canceled",
        json!({ "reason": "refunded" }),
        None,
    )
    .await?;
    tx.commit().await?;
    info!(%user_id, subscription = %subscription_id, "Fully refunded subscription canceled");
    after_change(state, user_id, change).await;
    Ok(())
}

/// `charge.dispute.*`: the ledger records the disputed amount; a new
/// dispute also cancels the subscription it paid for right away (so the
/// disputed card isn't charged again) and tells the owner.
async fn handle_dispute(
    state: &AppState,
    payments: &Payments,
    event: &StripeEvent,
    dispute: &Value,
) -> Result<(), ApiError> {
    let Some(disputed) = finance::apply_dispute(state, dispute).await? else {
        return Ok(());
    };
    if event.kind != "charge.dispute.created" {
        return Ok(());
    }
    let (Some(user_id), Some(subscription_id)) = (disputed.user_id, disputed.subscription_id)
    else {
        return Ok(());
    };
    payments.gateway.cancel_now(&subscription_id).await?;
    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, &subscription_id).await?;
    let change = end_paid_subscription(
        &mut tx,
        user_id,
        &subscription_id,
        "disputed",
        json!({ "dispute_status": disputed.status, "disputed_cents": disputed.amount }),
        None,
    )
    .await?;
    tx.commit().await?;
    warn!(%user_id, subscription = %subscription_id, "Payment disputed; subscription canceled");
    after_change(state, user_id, change).await;
    Ok(())
}

// ---------------------------------------------------------------------
// Withdrawal and refunds
// ---------------------------------------------------------------------

fn withdrawal_not_eligible(eligible_until: Option<NaiveDateTime>) -> ApiError {
    ApiError::rule_with_meta(
        StatusCode::CONFLICT,
        codes::WITHDRAWAL_NOT_ELIGIBLE,
        "The 7-day withdrawal period of this subscription is over. You can still cancel it in the billing portal.",
        json!({ "eligible_until": eligible_until.map(|t| t.and_utc()) }),
    )
}

/// `POST /billing/withdraw`: the buyer's right to withdraw (CDC art. 49,
/// Decreto 7.962 art. 5). Within 7 days of the subscription's first paid
/// invoice (or of a yearly renewal charge), cancels it now and refunds
/// every charge in that window in full, then confirms by e-mail.
pub async fn withdraw(
    state: &AppState,
    user_id: Uuid,
    ip: Option<String>,
) -> Result<WithdrawResponse, ApiError> {
    refund_and_end(state, user_id, None, "withdrawal", ip).await
}

/// Staff: cancels the account's paid subscription now and refunds it:
/// the charges within the withdrawal window when inside it, else the
/// latest charge in full. Audited with `actor`. For the staff endpoint
/// `POST /admin/users/{id}/subscription/refund`.
pub async fn refund_and_cancel(
    state: &AppState,
    user_id: Uuid,
    actor: Uuid,
    reason: &str,
    ip: Option<String>,
) -> Result<WithdrawResponse, ApiError> {
    refund_and_end(state, user_id, Some(actor), reason, ip).await
}

/// Which paid invoices a withdrawal (or a staff refund) refunds, oldest
/// first. `Err` with the deadline when a withdrawal isn't allowed.
fn invoices_to_refund(
    invoices: &[GatewayInvoice],
    yearly: bool,
    at: NaiveDateTime,
    staff: bool,
) -> Result<Vec<GatewayInvoice>, Option<NaiveDateTime>> {
    let paid_at = |i: &GatewayInvoice| i.paid_at.and_then(unix_to_naive).unwrap_or(at);
    let mut invoices: Vec<GatewayInvoice> = invoices.to_vec();
    invoices.sort_by_key(paid_at);
    let (Some(first), Some(latest)) = (invoices.first(), invoices.last()) else {
        // Nothing was charged yet (a card-on-file trial): nothing to
        // refund; the buyer cancels in the portal.
        return if staff { Ok(Vec::new()) } else { Err(None) };
    };
    let deadline = billing::withdrawal_deadline(paid_at(first), paid_at(latest), yearly);
    let window = Duration::days(WITHDRAWAL_DAYS);
    if at <= paid_at(first) + window {
        Ok(invoices)
    } else if at <= deadline {
        // A yearly renewal: the charges of the last 7 days.
        Ok(invoices
            .into_iter()
            .filter(|i| paid_at(i) + window >= at)
            .collect())
    } else if staff {
        Ok(vec![latest.clone()])
    } else {
        Err(Some(deadline))
    }
}

async fn refund_and_end(
    state: &AppState,
    user_id: Uuid,
    actor: Option<Uuid>,
    reason: &str,
    ip: Option<String>,
) -> Result<WithdrawResponse, ApiError> {
    let payments = payments(state)?;
    let staff = actor.is_some();
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT external_ref, billing_interval FROM subscriptions
         WHERE user_id = $1 AND source = 'payment' AND external_ref IS NOT NULL
           AND status IN ('active', 'past_due')",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((subscription_id, interval)) = row else {
        return Err(no_paid_subscription());
    };

    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, &subscription_id).await?;
    // The row itself, locked: still that subscription, still live.
    let still: Option<String> = sqlx::query_scalar(
        "SELECT plan_code FROM subscriptions
         WHERE user_id = $1 AND source = 'payment' AND external_ref = $2
           AND status IN ('active', 'past_due')
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(&subscription_id)
    .fetch_optional(&mut *tx)
    .await?;
    if still.is_none() {
        return Err(no_paid_subscription());
    }

    let timestamp = now();
    let invoices = payments
        .gateway
        .list_subscription_invoices(&subscription_id)
        .await?;
    let to_refund = invoices_to_refund(
        &invoices,
        interval.as_deref() == Some("yearly"),
        timestamp,
        staff,
    )
    .map_err(withdrawal_not_eligible)?;

    let mut refunded = 0;
    let mut refunds = Vec::new();
    let currency = to_refund
        .first()
        .or(invoices.first())
        .map(|i| i.currency.to_ascii_lowercase())
        .unwrap_or_else(|| "brl".to_string());
    for invoice in &to_refund {
        let Some(payment_intent) = &invoice.payment_intent_id else {
            warn!(invoice = %invoice.id, "Paid invoice without a payment; not refunded");
            continue;
        };
        match payments
            .gateway
            .refund(
                payment_intent,
                None,
                &format!("setlyst-withdraw-{}", invoice.id),
            )
            .await
        {
            Ok(refund) => {
                if refund.counts() {
                    refunded += refund.amount;
                }
                refunds.push(refund);
            }
            Err(e) if already_refunded(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    payments.gateway.cancel_now(&subscription_id).await?;

    let kind = if staff { "refunded" } else { "withdrawn" };
    let change = end_paid_subscription(
        &mut tx,
        user_id,
        &subscription_id,
        kind,
        json!({
            "refunded_cents": refunded,
            "currency": currency,
            "reason": reason,
            "invoices": to_refund.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        }),
        actor,
    )
    .await?;
    tx.commit().await?;

    // The ledger, best effort (the webhook and the staff sync catch up).
    for invoice in &to_refund {
        if let Err(e) = finance::record_invoice(state, invoice).await {
            error!(invoice = %invoice.id, error = %e, "Could not record a refunded invoice");
        }
    }
    for refund in &refunds {
        if let Err(e) = finance::upsert_refund(state, refund).await {
            error!(refund = %refund.id, error = %e, "Could not record a refund");
        }
    }

    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    let actor_id = actor.unwrap_or(user_id);
    let actor_name: Option<String> = match actor {
        Some(actor) => {
            sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
                .bind(actor)
                .fetch_optional(&state.db)
                .await?
        }
        None => username.clone(),
    };
    AuditEvent::new(if staff {
        AUDIT_REFUNDED
    } else {
        AUDIT_WITHDRAWN
    })
    .actor(actor_id, actor_name.as_deref().unwrap_or_default())
    .target("user", user_id, username.as_deref().unwrap_or_default())
    .meta(json!({
        "subscription": subscription_id,
        "refunded_cents": refunded,
        "currency": currency,
        "reason": reason,
    }))
    .ip(&ip)
    .record(&*state.audit_repo)
    .await;
    info!(%user_id, subscription = %subscription_id, refunded, kind, "Paid subscription refunded and canceled");
    after_change(state, user_id, change).await;
    Ok(WithdrawResponse {
        refunded_cents: refunded,
        currency,
    })
}

// ---------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------

/// Paid subscriptions whose period ended, or whose failed renewal ran out
/// of grace, are checked with the provider before they lose their plan (a
/// renewal webhook may have been lost). A subscription the provider no
/// longer has, or one it still reports as failing after the grace period
/// (then canceled there, as the Terms promise no further charge), expires
/// here. When the provider can't be reached, the row waits for the next
/// run. Returns how many expired.
pub async fn reconcile_due(state: &AppState) -> Result<u64, ApiError> {
    let timestamp = now();
    let due: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT user_id, external_ref FROM subscriptions
         WHERE source = 'payment' AND status IN ('trialing', 'active', 'past_due')
           AND ((current_period_end IS NOT NULL AND current_period_end <= $1)
                OR (status = 'past_due' AND past_due_since IS NOT NULL AND past_due_since <= $2))",
    )
    .bind(timestamp)
    .bind(timestamp - Duration::days(PAYMENT_GRACE_DAYS))
    .fetch_all(&state.db)
    .await?;
    let mut expired = 0;
    for (user_id, external_ref) in due {
        let result = match (&state.payments, &external_ref) {
            (Some(payments), Some(external_ref)) => {
                reconcile_one(state, payments, user_id, external_ref).await
            }
            // No provider to ask: expire once past the grace period.
            _ => expire_lapsed(state, user_id, external_ref.as_deref()).await,
        };
        match result {
            Ok(true) => expired += 1,
            Ok(false) => {}
            Err(e) => {
                warn!(%user_id, error = %e, "Could not reconcile a paid subscription; retrying next hour")
            }
        }
    }
    Ok(expired)
}

/// `true` when the subscription expired.
async fn reconcile_one(
    state: &AppState,
    payments: &Payments,
    user_id: Uuid,
    external_ref: &str,
) -> Result<bool, ApiError> {
    let mut tx = state.db.begin().await?;
    lock_provider_subscription(&mut tx, external_ref).await?;
    let fetched = match payments.gateway.get_subscription(external_ref).await {
        Ok(subscription) => Some(subscription),
        Err(e) if e.is_missing() => None,
        Err(e) => return Err(e.into()),
    };
    let mut change = None;
    if let Some(mut subscription) = fetched {
        let out_of_grace: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM subscriptions
                            WHERE user_id = $1 AND external_ref = $2 AND status = 'past_due'
                              AND past_due_since <= $3)",
        )
        .bind(user_id)
        .bind(external_ref)
        .bind(now() - Duration::days(PAYMENT_GRACE_DAYS))
        .fetch_one(&mut *tx)
        .await?;
        if out_of_grace && matches!(subscription.status.as_str(), "past_due" | "unpaid") {
            warn!(%user_id, subscription = %external_ref, "Renewal still failing after the grace period; canceling at the provider");
            payments.gateway.cancel_now(external_ref).await?;
            subscription = payments.gateway.get_subscription(external_ref).await?;
        }
        change = mirror_in(
            state,
            &mut tx,
            user_id,
            &subscription,
            &MirrorOptions::default(),
        )
        .await?
        .change;
    }
    let expired = expire_lapsed_in(&mut tx, user_id, Some(external_ref)).await?;
    tx.commit().await?;
    let lapsed = expired.is_some();
    after_change(state, user_id, change).await;
    after_expiry(state, user_id, expired).await;
    Ok(lapsed)
}

async fn expire_lapsed(
    state: &AppState,
    user_id: Uuid,
    external_ref: Option<&str>,
) -> Result<bool, ApiError> {
    let mut tx = state.db.begin().await?;
    let expired = expire_lapsed_in(&mut tx, user_id, external_ref).await?;
    tx.commit().await?;
    let lapsed = expired.is_some();
    after_expiry(state, user_id, expired).await;
    Ok(lapsed)
}

/// Expires the account's paid subscription `external_ref` when it is past
/// its renewal or payment grace. Returns its plan when it did.
async fn expire_lapsed_in(
    tx: &mut PgConnection,
    user_id: Uuid,
    external_ref: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let timestamp = now();
    let expired: Option<(String, SubscriptionStatus)> = sqlx::query_as(
        "WITH old AS (
             SELECT user_id, status FROM subscriptions
             WHERE user_id = $1 AND source = 'payment' AND external_ref IS NOT DISTINCT FROM $2
               AND status IN ('trialing', 'active', 'past_due')
               AND ((current_period_end IS NOT NULL AND current_period_end <= $3)
                    OR (status = 'past_due' AND past_due_since IS NOT NULL AND past_due_since <= $4))
             FOR UPDATE
         )
         UPDATE subscriptions s SET status = 'expired', updated_at = $5
         FROM old WHERE s.user_id = old.user_id
         RETURNING s.plan_code, old.status",
    )
    .bind(user_id)
    .bind(external_ref)
    .bind(timestamp - Duration::days(RENEWAL_GRACE_DAYS))
    .bind(timestamp - Duration::days(PAYMENT_GRACE_DAYS))
    .bind(timestamp)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((plan_code, from_status)) = expired else {
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO subscription_events (id, user_id, kind, from_plan, to_plan, from_status, to_status,
                                          data, created_at)
         VALUES ($1, $2, 'expired', $3, $3, $4, 'expired', $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(&plan_code)
    .bind(from_status)
    .bind(json!({ "source": SubscriptionSource::Payment, "external_ref": external_ref }))
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    Ok(Some(plan_code))
}

async fn after_expiry(state: &AppState, user_id: Uuid, plan_code: Option<String>) {
    let Some(plan_code) = plan_code else {
        return;
    };
    info!(%user_id, plan = %plan_code, "Paid subscription expired");
    notify_all(
        state,
        vec![
            crate::models::notification::Notification::subscription_changed(
                user_id,
                "expired",
                Some(&plan_code),
                Some("expired"),
                Some(now()),
            ),
        ],
    )
    .await;
    if let Err(e) = revoke_shares_if_lost(state, user_id).await {
        warn!(%user_id, error = %e, "Could not revoke public links");
    }
}

/// Daily: walks every subscription at the provider and mirrors the ones
/// whose local copy differs (a webhook lost or never configured). Returns
/// how many drifted.
pub async fn reconcile_all(state: &AppState) -> Result<u64, ApiError> {
    let Some(payments) = &state.payments else {
        return Ok(0);
    };
    let mut drifted = 0;
    let mut after: Option<String> = None;
    for _ in 0..MAX_RECONCILE_PAGES {
        let page = payments
            .gateway
            .list_subscriptions(None, after.as_deref())
            .await?;
        for subscription in &page.subscriptions {
            if subscription.plan_code.is_none() {
                continue;
            }
            let Some(user_id) = owner_of(state, subscription, None).await? else {
                continue;
            };
            if in_sync(state, user_id, subscription).await? {
                continue;
            }
            match sync_subscription(
                state,
                payments,
                &subscription.id,
                None,
                &MirrorOptions::default(),
            )
            .await
            {
                Ok(_) => {
                    drifted += 1;
                    warn!(%user_id, subscription = %subscription.id, status = %subscription.status, "Subscription out of sync with Stripe; mirrored");
                }
                Err(e) => {
                    warn!(%user_id, subscription = %subscription.id, error = %e, "Could not mirror a drifted subscription")
                }
            }
        }
        match (page.has_more, page.last_id) {
            (true, Some(last)) => after = Some(last),
            _ => break,
        }
    }
    Ok(drifted)
}

/// `true` when the local row already says what the provider says about
/// `subscription` (or the subscription is an old one that doesn't matter).
async fn in_sync(
    state: &AppState,
    user_id: Uuid,
    subscription: &GatewaySubscription,
) -> Result<bool, ApiError> {
    let Some(snapshot) = snapshot_of(subscription) else {
        return Ok(true);
    };
    #[allow(clippy::type_complexity)]
    let row: Option<(
        Option<String>,
        SubscriptionStatus,
        Option<NaiveDateTime>,
        bool,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT external_ref, status, current_period_end, cancel_at_period_end, provider_status, plan_code
         FROM subscriptions WHERE user_id = $1 AND source = 'payment'",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let live = snapshot.status.is_some_and(|s| s.is_live());
    Ok(match row {
        Some((external_ref, status, period_end, canceling, provider_status, plan_code))
            if external_ref.as_deref() == Some(subscription.id.as_str()) =>
        {
            Some(status) == snapshot.status
                && (!live || period_end == snapshot.current_period_end)
                && canceling == snapshot.cancel_at_period_end
                && provider_status.as_deref() == Some(snapshot.provider_status.as_str())
                && plan_code == snapshot.plan_code
        }
        // Not the mirrored one: only a live one needs a look.
        _ => !live,
    })
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
            status: "trialing".into(),
            item_id: "si_1".into(),
            price_id: "price_1".into(),
            interval: Some(BillingInterval::Monthly),
            current_period_end: Some(1_800_000_000),
            trial_end: Some(1_800_000_000),
            unit_amount: Some(3990),
            ..Default::default()
        };
        assert!(snapshot_of(&subscription).is_none());
        let snapshot = snapshot_of(&GatewaySubscription {
            plan_code: Some("pro".into()),
            ..subscription
        })
        .unwrap();
        assert_eq!(snapshot.status, Some(SubscriptionStatus::Active));
        assert_eq!(snapshot.provider_status, "trialing");
        assert_eq!(snapshot.unit_amount, Some(3990));
        assert_eq!(
            snapshot.current_period_end.unwrap().and_utc().timestamp(),
            1_800_000_000
        );
        assert!(snapshot.trial_end.is_some());
    }

    fn invoice(id: &str, days_ago: i64) -> GatewayInvoice {
        GatewayInvoice {
            id: id.into(),
            customer_id: None,
            subscription_id: Some("sub_1".into()),
            user_id: None,
            payment_intent_id: Some(format!("pi_{id}")),
            plan_code: Some("pro".into()),
            interval: None,
            amount_paid: 3990,
            currency: "BRL".into(),
            paid_at: Some(Utc::now().timestamp() - days_ago * 86_400),
        }
    }

    #[test]
    fn withdrawals_refund_the_charges_of_the_window() {
        let at = now();
        let ids =
            |invoices: Vec<GatewayInvoice>| invoices.into_iter().map(|i| i.id).collect::<Vec<_>>();
        // Within 7 days of the first charge: everything (a plan change
        // included).
        let fresh = [invoice("in_2", 1), invoice("in_1", 3)];
        assert_eq!(
            ids(invoices_to_refund(&fresh, false, at, false).unwrap()),
            ["in_1", "in_2"]
        );
        // A monthly subscription past its first week: not eligible.
        let old = [invoice("in_2", 2), invoice("in_1", 33)];
        assert!(matches!(
            invoices_to_refund(&old, false, at, false),
            Err(Some(_))
        ));
        // A yearly renewal charged 2 days ago: that charge only.
        let renewal = [invoice("in_2", 2), invoice("in_1", 367)];
        assert_eq!(
            ids(invoices_to_refund(&renewal, true, at, false).unwrap()),
            ["in_2"]
        );
        // Staff outside the window refund the latest charge.
        assert_eq!(
            ids(invoices_to_refund(&old, false, at, true).unwrap()),
            ["in_2"]
        );
        // Nothing charged yet (card-on-file trial).
        assert!(matches!(
            invoices_to_refund(&[], false, at, false),
            Err(None)
        ));
        assert!(invoices_to_refund(&[], false, at, true).unwrap().is_empty());
    }

    #[test]
    fn terms_messages_link_the_subscription_terms_in_each_language() {
        for (locale, word) in [("pt-BR", "7 dias"), ("en", "7 days"), ("es", "7 días")] {
            let message = terms_message(locale);
            assert!(
                message.contains(&format!("/{locale}/legal/subscription")),
                "{message}"
            );
            assert!(message.contains(word), "{message}");
            // Stripe limits this text to 1200 characters.
            assert!(message.chars().count() < 1200);
        }
    }

    #[test]
    fn pending_changes_ask_for_authentication_or_report_a_decline() {
        let needs_action = GatewaySubscription {
            has_pending_update: true,
            latest_invoice_status: Some("open".into()),
            latest_payment_status: Some("requires_action".into()),
            hosted_invoice_url: Some("https://invoice.stripe.com/i/1".into()),
            ..Default::default()
        };
        assert!(matches!(
            pending_change_error(&needs_action),
            ApiError::Rule {
                code: codes::PAYMENT_ACTION_REQUIRED,
                ..
            }
        ));
        let declined = GatewaySubscription {
            latest_payment_status: Some("requires_payment_method".into()),
            ..needs_action
        };
        assert!(matches!(
            pending_change_error(&declined),
            ApiError::Rule {
                code: codes::PAYMENT_DECLINED,
                ..
            }
        ));
    }
}
