//! Plans and subscriptions: the caller's billing (`/billing`) and the
//! staff console (`/admin/billing`, `/admin/plans`, `/admin/promo-codes`,
//! `/admin/promotions`, `/admin/users/{id}/subscription|credits`).

use crate::{
    database::{AppState, repositories::audit_repository::AuditEvent},
    errors::api_error::ApiError,
    models::{
        PaginatedResponse, PaginationQuery,
        audit::actions,
        auth::access::{AccessControl, ClientIp},
        billing::{
            AdjustCreditsPayload, AdminSubscriptionView, BillingMe, BillingOverview,
            BillingPageQuery, BillingSettings, CheckoutPayload, CreatePromoCodePayload,
            CreatePromotionPayload, CreditEntry, GrantSubscriptionPayload, GrantTrialsPayload,
            GrantTrialsResponse, Plan, PromoCode, PromoListQuery, PromoRedemption, Promotion,
            RedeemCodePayload, RedeemResponse, RedeemRewardPayload, RedirectResponse,
            ReferralEntry, SubscriptionEvent, SubscriptionSource, UpdatePromoCodePayload,
            UpdatePromotionPayload, UpsertPlanPayload, WithdrawResponse, check_plan_code,
        },
        finance::{FinanceOverview, FinanceSyncPayload, FinanceSyncResult},
        notification::Notification,
        resolve_page,
    },
    services::{
        account::too_many_attempts,
        billing::{self, add_credits, grant_plan_time, lock_user_credits},
        finance,
        notifier::{notify, notify_all},
        payments,
    },
    utils::{codes::referral_code, rate_limit::SlidingWindowLimiter},
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use std::{sync::LazyLock, time::Duration};
use tracing::info;
use utoipa::IntoParams;
use uuid::Uuid;
use validator::Validate;

/// Promo-code attempts per account per hour (valid or not), to stop code
/// guessing.
static REDEEM_LIMITER: LazyLock<SlidingWindowLimiter<Uuid>> =
    LazyLock::new(|| SlidingWindowLimiter::new(10, Duration::from_secs(3600)));

fn validation(field: &'static str, message: String) -> ApiError {
    let mut errors = validator::ValidationErrors::new();
    let mut error = validator::ValidationError::new("invalid");
    error.message = Some(message.into());
    errors.add(field, error);
    ApiError::ValidationError(errors)
}

// ---------------------------------------------------------------------
// The caller
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/billing/me",
    tags = ["Billing"],
    summary = "The caller's plan, features, credits and referrals.",
    description = "`plan` is the plan in effect (`null` without one); `features` is what the caller may use right now (everything while plans are not enforced). `withdrawal_eligible_until` is set while the paid subscription can still be withdrawn from with a full refund (`POST /billing/withdraw`); `past_due_since` while its last renewal charge is failing (the plan is kept for 14 days from then).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Billing state.", body = BillingMe))
)]
pub async fn get_me(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(billing::billing_me(&state, access.user_id()).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/redeem",
    tags = ["Billing"],
    summary = "Redeem a promo code.",
    description = "Applies the code (plan time, trial extension, credits, or a discount stored for the next payment) and returns the updated billing state plus `redemption`. Errors: `PROMO_CODE_INVALID`, `PROMO_CODE_EXPIRED`, `PROMO_CODE_EXHAUSTED`, `PROMO_CODE_ALREADY_REDEEMED`, `PROMO_CODE_NOT_ELIGIBLE`. At most 10 attempts per hour (`TOO_MANY_ATTEMPTS`).",
    request_body = RedeemCodePayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Redeemed.", body = RedeemResponse),
        (status = 400, description = "Invalid or expired code."),
        (status = 403, description = "Not eligible."),
        (status = 409, description = "Exhausted or already redeemed."),
        (status = 429, description = "Too many attempts."),
    )
)]
pub async fn redeem_code(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<RedeemCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    if let Err(retry) = REDEEM_LIMITER.check(&access.user_id()) {
        return Err(too_many_attempts(retry.as_secs() as i64));
    }
    let redemption = billing::redeem_promo_code(&state, access.user_id(), &payload.code).await?;
    Ok(Json(RedeemResponse {
        billing: billing::billing_me(&state, access.user_id()).await?,
        redemption,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/billing/credits",
    tags = ["Billing"],
    summary = "The caller's credit ledger, newest first.",
    params(BillingPageQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Ledger entries.", body = PaginatedResponse<CreditEntry>))
)]
pub async fn list_credits(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<BillingPageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let (items, total) = state
        .billing_repo
        .credit_ledger(access.user_id(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/credits/redeem",
    tags = ["Billing"],
    summary = "Spend credits on a reward.",
    description = "Debits the reward's cost and grants its plan time. `REWARD_NOT_FOUND`, `INSUFFICIENT_CREDITS` (`meta: { balance, required }`).",
    request_body = RedeemRewardPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Redeemed.", body = BillingMe),
        (status = 404, description = "Unknown reward."),
        (status = 409, description = "Not enough credits."),
    )
)]
pub async fn redeem_reward(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<RedeemRewardPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payload.validate()?;
    billing::redeem_reward(&state, access.user_id(), payload.reward_id.trim()).await?;
    Ok(Json(billing::billing_me(&state, access.user_id()).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/billing/referrals",
    tags = ["Billing"],
    summary = "Accounts the caller referred.",
    params(BillingPageQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Referrals.", body = PaginatedResponse<ReferralEntry>))
)]
pub async fn list_referrals(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<BillingPageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (page, per_page) = resolve_page(query.page, query.per_page, 20);
    let (items, total) = state
        .billing_repo
        .referrals(access.user_id(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    get,
    path = "/api/v1/billing/history",
    tags = ["Billing"],
    summary = "Changes to the caller's subscription, newest first (last 100).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Events.", body = [SubscriptionEvent]))
)]
pub async fn history(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .billing_repo
            .subscription_events(access.user_id(), 100)
            .await?,
    ))
}

// ---------------------------------------------------------------------
// Payments
// ---------------------------------------------------------------------

/// Checkouts, plan changes and portal sessions per account per hour: each
/// one calls the payment provider.
static PAYMENT_LIMITER: LazyLock<SlidingWindowLimiter<Uuid>> =
    LazyLock::new(|| SlidingWindowLimiter::new(20, Duration::from_secs(3600)));

fn payment_attempt(access: &AccessControl) -> Result<(), ApiError> {
    PAYMENT_LIMITER
        .check(&access.user_id())
        .map_err(|retry| too_many_attempts(retry.as_secs() as i64))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/checkout",
    tags = ["Billing"],
    summary = "Start a paid subscription.",
    description = "Answers the URL of a checkout page hosted by the payment provider; send the browser there. The buyer must accept the Subscription Terms on that page (cards only). It returns to `/dashboard/settings?checkout=success` (or `=canceled`) and the subscription shows up in `/billing/me` once the provider confirms the payment (usually within seconds). A running trial carries over: the first charge waits for its end. A running promotion or a redeemed `discount` code comes off the first charge. Starting a new checkout expires the pages of earlier ones. Errors: `PAYMENTS_UNAVAILABLE`, `BILLING_NOT_ENFORCED`, `PLAN_NOT_FOUND`, `PLAN_NOT_PURCHASABLE`, `EMAIL_NOT_VERIFIED`, `PAID_SUBSCRIPTION_ACTIVE` (also when the provider already has a live subscription for the account; use `/billing/subscription/change`), `PAYMENT_PROVIDER_ERROR`.",
    request_body = CheckoutPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Checkout page.", body = RedirectResponse),
        (status = 403, description = "E-mail not verified."),
        (status = 404, description = "Unknown plan."),
        (status = 409, description = "Not enforced, or already subscribed."),
        (status = 502, description = "The payment provider failed."),
        (status = 503, description = "Payments not configured."),
    )
)]
pub async fn checkout(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CheckoutPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payment_attempt(&access)?;
    Ok(Json(
        payments::start_checkout(&state, access.user_id(), &payload).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/subscription/change",
    tags = ["Billing"],
    summary = "Switch the paid subscription to another plan or interval.",
    description = "Charges the prorated difference now; the change only applies if that charge succeeds (`PAYMENT_DECLINED` otherwise, with the plan unchanged). When the bank asks the customer to confirm the charge (3-D Secure) the answer is `PAYMENT_ACTION_REQUIRED` with `meta.hosted_invoice_url`: send the browser there; the new plan applies once confirmed. Answers the updated billing state. Errors: `NO_PAID_SUBSCRIPTION`, `PLAN_ALREADY_ACTIVE`, `SUBSCRIPTION_PAST_DUE`, `SUBSCRIPTION_CANCELING` (renew it in the portal first), plus those of `/billing/checkout`.",
    request_body = CheckoutPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Plan changed.", body = BillingMe),
        (status = 402, description = "The charge was declined or needs the customer's confirmation."),
        (status = 409, description = "Nothing to change."),
    )
)]
pub async fn change_subscription(
    State(state): State<AppState>,
    access: AccessControl,
    Json(payload): Json<CheckoutPayload>,
) -> Result<impl IntoResponse, ApiError> {
    payment_attempt(&access)?;
    Ok(Json(
        payments::change_plan(&state, access.user_id(), &payload).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/portal",
    tags = ["Billing"],
    summary = "Open the billing portal.",
    description = "Answers the URL of the payment provider's portal, where the caller updates the card, downloads invoices and cancels or renews the subscription. Errors: `PAYMENTS_UNAVAILABLE`, `NO_PAID_SUBSCRIPTION` (never paid).",
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Portal page.", body = RedirectResponse),
        (status = 409, description = "No billing account."),
    )
)]
pub async fn portal(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    payment_attempt(&access)?;
    Ok(Json(payments::open_portal(&state, access.user_id()).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/billing/withdraw",
    tags = ["Billing"],
    summary = "Withdraw from the paid subscription (7-day right, full refund).",
    description = "Within 7 days of the subscription's first paid invoice (or of a yearly renewal charge; see `withdrawal_eligible_until` in `/billing/me`): cancels the subscription now, refunds every charge in that window in full and e-mails a confirmation. The account then has no plan; its content stays. Errors: `WITHDRAWAL_NOT_ELIGIBLE` (`meta.eligible_until`; cancel in the portal instead), `NO_PAID_SUBSCRIPTION`, `PAYMENTS_UNAVAILABLE`, `PAYMENT_PROVIDER_ERROR` (nothing is lost: repeat the request).",
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Withdrawn and refunded.", body = WithdrawResponse),
        (status = 409, description = "Outside the window, or no paid subscription."),
        (status = 502, description = "The payment provider failed."),
    )
)]
pub async fn withdraw(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
) -> Result<impl IntoResponse, ApiError> {
    payment_attempt(&access)?;
    Ok(Json(
        payments::withdraw(&state, access.user_id(), ip.0).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/webhooks/stripe",
    tags = ["Billing"],
    summary = "Stripe webhook.",
    description = "Called by Stripe, not by clients. The body must carry a valid `Stripe-Signature`. Redelivered events are acknowledged without effect.",
    responses(
        (status = 200, description = "Received."),
        (status = 400, description = "Bad signature or body."),
    )
)]
pub async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok());
    payments::handle_webhook(&state, &body, signature).await?;
    Ok(Json(json!({ "received": true })))
}

// ---------------------------------------------------------------------
// Staff: settings, plans, overview
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/billing/settings",
    tags = ["Billing admin"],
    summary = "Billing settings (admin).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Settings.", body = BillingSettings))
)]
pub async fn get_settings(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    Ok(Json(state.billing_repo.get_settings().await?))
}

/// Query of `PUT /admin/billing/settings`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SettingsQuery {
    /// Switch `enforced` off even though paid subscriptions are live.
    #[serde(default)]
    pub force: bool,
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/billing/settings",
    tags = ["Billing admin"],
    summary = "Replace the billing settings (admin).",
    description = "`trial_plan` and every reward's `plan` must exist (`PLAN_NOT_FOUND`); reward ids must be unique. Switching `enforced` on applies plan features and limits to every account: grant trials first (`POST /admin/billing/grant-trials`). Switching it off while paid subscriptions are live answers `BILLING_HAS_PAID_SUBSCRIPTIONS` (409, `meta.live`): those customers keep being charged; repeat with `?force=true` to do it anyway.",
    params(SettingsQuery),
    request_body = BillingSettings,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Saved settings.", body = BillingSettings),
        (status = 409, description = "Paid subscriptions are live."),
    )
)]
pub async fn update_settings(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Query(query): Query<SettingsQuery>,
    Json(payload): Json<BillingSettings>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let plans: Vec<String> = state
        .billing_repo
        .list_plans(false)
        .await?
        .into_iter()
        .map(|p| p.code)
        .collect();
    let plan_error = |code: &str| {
        ApiError::rule(
            StatusCode::NOT_FOUND,
            crate::errors::api_error::codes::PLAN_NOT_FOUND,
            format!("The plan '{code}' does not exist."),
        )
    };
    if !plans.contains(&payload.trial_plan) {
        return Err(plan_error(&payload.trial_plan));
    }
    let mut seen = std::collections::HashSet::new();
    for reward in &payload.rewards {
        if !plans.contains(&reward.plan) {
            return Err(plan_error(&reward.plan));
        }
        if !seen.insert(reward.id.as_str()) {
            return Err(validation(
                "rewards",
                format!("Duplicate reward id '{}'.", reward.id),
            ));
        }
    }

    let previous = state.billing_repo.get_settings().await?;
    if previous.enforced && !payload.enforced && !query.force {
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM subscriptions
             WHERE source = 'payment' AND status IN ('active', 'past_due')",
        )
        .fetch_one(&state.db)
        .await?;
        if live > 0 {
            return Err(ApiError::rule_with_meta(
                StatusCode::CONFLICT,
                crate::errors::api_error::codes::BILLING_HAS_PAID_SUBSCRIPTIONS,
                "Paid subscriptions are live and keep being charged. Confirm to switch plans off anyway.",
                json!({ "live": live }),
            ));
        }
    }
    state
        .billing_repo
        .set_settings(&payload, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::BILLING_SETTINGS_UPDATED)
        .meta(json!({ "from": previous, "to": payload }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(payload))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/billing/overview",
    tags = ["Billing admin"],
    summary = "Subscription and credit figures (staff).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Overview.", body = BillingOverview))
)]
pub async fn overview(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.billing_repo.overview().await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/finance",
    tags = ["Billing admin"],
    summary = "Revenue, subscribers and recent payments (admin).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Finance report.", body = FinanceOverview))
)]
pub async fn finance_overview(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    Ok(Json(finance::overview(&state).await?))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/finance/sync",
    tags = ["Billing admin"],
    summary = "Copy paid invoices and refunds from the payment provider into the ledger (admin). Repeat with the returned cursor until `done`.",
    request_body = FinanceSyncPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Sync finished.", body = FinanceSyncResult),
        (status = 409, description = "Payments are not configured."),
        (status = 502, description = "The payment provider could not be reached.")
    )
)]
pub async fn finance_sync(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<FinanceSyncPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let result = finance::sync_from_provider(&state, payload.cursor.as_deref()).await?;
    // One entry per sync, not per call of a multi-call run.
    if payload.cursor.is_none() {
        AuditEvent::by(&access, actions::FINANCE_SYNCED)
            .meta(json!({ "scanned": result.scanned, "imported": result.imported }))
            .ip(&ip.0)
            .record(&*state.audit_repo)
            .await;
    }
    Ok(Json(result))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/billing/grant-trials",
    tags = ["Billing admin"],
    summary = "Start a trial for every account without a subscription (admin).",
    request_body = GrantTrialsPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Trials started.", body = GrantTrialsResponse))
)]
pub async fn grant_trials(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<GrantTrialsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let granted = billing::grant_trials(&state, payload.days, access.user_id()).await?;
    AuditEvent::by(&access, actions::BILLING_TRIALS_GRANTED)
        .meta(json!({ "days": payload.days, "granted": granted }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(GrantTrialsResponse { granted }))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/plans",
    tags = ["Billing admin"],
    summary = "Every plan, public or not (staff).",
    description = "Read-only for moderators; only admins can change plans (`PUT /admin/plans/{code}`).",
    security(("jwt_token" = [])),
    responses((status = 200, description = "Plans.", body = [Plan]))
)]
pub async fn list_plans(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    Ok(Json(state.billing_repo.list_plans(false).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/plans/{code}",
    tags = ["Billing admin"],
    summary = "One plan, public or not (staff).",
    params(("code" = String, Path, description = "Plan code")),
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "The plan.", body = Plan),
        (status = 404, description = "No such plan (`PLAN_NOT_FOUND`)."),
    )
)]
pub async fn get_plan(
    State(state): State<AppState>,
    access: AccessControl,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    let plan = state.billing_repo.find_plan(&code).await?.ok_or_else(|| {
        ApiError::rule(
            StatusCode::NOT_FOUND,
            crate::errors::api_error::codes::PLAN_NOT_FOUND,
            format!("The plan '{code}' does not exist."),
        )
    })?;
    Ok(Json(plan))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/plans/{code}",
    tags = ["Billing admin"],
    summary = "Create or update a plan (admin).",
    description = "Omitted fields keep their value. A new plan needs `name` (`en` and `pt-BR`). `limits` is a full `QuotaLimits`; `features` accepts known feature keys only.",
    params(("code" = String, Path, description = "Plan code")),
    request_body = UpsertPlanPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Saved plan.", body = Plan))
)]
pub async fn upsert_plan(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(code): Path<String>,
    Json(payload): Json<UpsertPlanPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    check_plan_code(&code).map_err(|e| {
        let mut errors = validator::ValidationErrors::new();
        errors.add("code", e);
        ApiError::ValidationError(errors)
    })?;
    payload.validate()?;
    let previous = state.billing_repo.find_plan(&code).await?;
    let plan = state
        .billing_repo
        .upsert_plan(&code, &payload, access.user_id())
        .await?
        .ok_or_else(|| validation("name", "A new plan needs a name.".into()))?;
    AuditEvent::by(&access, actions::BILLING_PLAN_UPDATED)
        .meta(json!({ "code": code, "created": previous.is_none(), "changes": payload }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(plan))
}

// ---------------------------------------------------------------------
// Staff: a user's subscription and credits
// ---------------------------------------------------------------------

async fn target_label(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
        .user_repo
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NotFound)?
        .username)
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/users/{id}/subscription",
    tags = ["Billing admin"],
    summary = "A user's subscription, history and credit balance (staff).",
    params(("id" = Uuid, Path, description = "User UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Subscription.", body = AdminSubscriptionView))
)]
pub async fn get_user_subscription(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_staff()?;
    state.user_repo.exists(id).await?;
    Ok(Json(AdminSubscriptionView {
        subscription: state.billing_repo.get_subscription(id).await?,
        events: state.billing_repo.subscription_events(id, 100).await?,
        credits_balance: state.billing_repo.credit_balance(id).await?,
    }))
}

#[utoipa::path(
    put,
    path = "/api/v1/admin/users/{id}/subscription",
    tags = ["Billing admin"],
    summary = "Grant a complimentary plan (admin).",
    description = "`days: null` = open-ended. Extends the period when the same plan is already in effect.",
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = GrantSubscriptionPayload,
    security(("jwt_token" = [])),
    responses(
        (status = 200, description = "Granted.", body = AdminSubscriptionView),
        (status = 404, description = "Unknown user or plan."),
    )
)]
pub async fn grant_user_subscription(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<GrantSubscriptionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let label = target_label(&state, id).await?;
    let note = payload
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let mut tx = state.db.begin().await?;
    let outcome = grant_plan_time(
        &mut tx,
        id,
        payload.plan_code.trim(),
        payload.days,
        SubscriptionSource::Admin,
        Some(access.user_id()),
        note,
        json!({}),
    )
    .await?;
    tx.commit().await?;
    notify(&state, outcome.notification(id)).await;
    AuditEvent::by(&access, actions::USER_SUBSCRIPTION_GRANTED)
        .target("user", id, &label)
        .meta(json!({ "plan": outcome.plan_code, "days": payload.days, "until": outcome.current_period_end, "note": note }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    get_user_subscription(State(state), access, Path(id)).await
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/users/{id}/subscription",
    tags = ["Billing admin"],
    summary = "End a user's subscription now (admin).",
    params(("id" = Uuid, Path, description = "User UUID")),
    security(("jwt_token" = [])),
    responses(
        (status = 204, description = "Revoked."),
        (status = 404, description = "No subscription."),
    )
)]
pub async fn revoke_user_subscription(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let label = target_label(&state, id).await?;
    if !billing::revoke_subscription(&state, id, access.user_id()).await? {
        return Err(ApiError::NotFound);
    }
    AuditEvent::by(&access, actions::USER_SUBSCRIPTION_REVOKED)
        .target("user", id, &label)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/users/{id}/credits",
    tags = ["Billing admin"],
    summary = "Adjust a user's credits (admin).",
    description = "`amount` from -100000 to 100000 (not zero). The balance can't go below zero (`INSUFFICIENT_CREDITS`).",
    params(("id" = Uuid, Path, description = "User UUID")),
    request_body = AdjustCreditsPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated subscription view.", body = AdminSubscriptionView))
)]
pub async fn adjust_user_credits(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<AdjustCreditsPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let label = target_label(&state, id).await?;
    let mut tx = state.db.begin().await?;
    lock_user_credits(&mut tx, id).await?;
    if payload.amount < 0 {
        let balance = billing::credit_balance(&mut tx, id).await?;
        if balance + i64::from(payload.amount) < 0 {
            return Err(ApiError::rule_with_meta(
                StatusCode::CONFLICT,
                crate::errors::api_error::codes::INSUFFICIENT_CREDITS,
                "The balance can't go below zero.",
                json!({ "balance": balance, "required": -payload.amount }),
            ));
        }
    }
    add_credits(
        &mut tx,
        id,
        payload.amount,
        "admin_adjustment",
        None,
        Some(payload.note.trim()),
        Some(access.user_id()),
    )
    .await?;
    tx.commit().await?;
    if payload.amount > 0 {
        notify_all(
            &state,
            vec![Notification::credits_granted(
                id,
                payload.amount,
                "admin_adjustment",
            )],
        )
        .await;
    }
    AuditEvent::by(&access, actions::USER_CREDITS_ADJUSTED)
        .target("user", id, &label)
        .meta(json!({ "amount": payload.amount, "note": payload.note.trim() }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    get_user_subscription(State(state), access, Path(id)).await
}

// ---------------------------------------------------------------------
// Staff: promo codes
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/promo-codes",
    tags = ["Billing admin"],
    summary = "Promo codes (admin).",
    params(PromoListQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Promo codes.", body = PaginatedResponse<PromoCode>))
)]
pub async fn list_promo_codes(
    State(state): State<AppState>,
    access: AccessControl,
    Query(query): Query<PromoListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let (page, per_page) = resolve_page(query.page, query.per_page, 25);
    let search = crate::models::admin::AdminListQuery {
        q: query.q.clone(),
        ..Default::default()
    }
    .search_pattern();
    let (items, total) = state
        .billing_repo
        .list_promo_codes(search.as_deref(), page, per_page)
        .await?;
    Ok(Json(PaginatedResponse::new(items, total, page, per_page)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/promo-codes",
    tags = ["Billing admin"],
    summary = "Create a promo code (admin).",
    description = "Kind-dependent fields: `plan_grant` needs `plan_code` + `duration_days`; `trial_extension` needs `duration_days`; `credits` needs `credits`; `discount` needs `discount_percent`. The code is generated when omitted and stored upper-case (`ALREADY_EXISTS` when taken).",
    request_body = CreatePromoCodePayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Created.", body = PromoCode))
)]
pub async fn create_promo_code(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreatePromoCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    payload
        .check_kind_fields()
        .map_err(|m| validation("kind", m))?;
    if let Some(plan) = &payload.plan_code
        && state.billing_repo.find_plan(plan).await?.is_none()
    {
        return Err(ApiError::rule(
            StatusCode::NOT_FOUND,
            crate::errors::api_error::codes::PLAN_NOT_FOUND,
            "This plan does not exist.",
        ));
    }
    let code = payload
        .code
        .as_deref()
        .map(|c| c.trim().to_uppercase())
        .unwrap_or_else(referral_code);
    let promo = state
        .billing_repo
        .create_promo_code(&code, &payload, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::PROMO_CREATED)
        .target("promo_code", promo.id, &promo.code)
        .meta(json!({ "kind": promo.kind, "max_redemptions": promo.max_redemptions }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::CREATED, Json(promo)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/promo-codes/{id}",
    tags = ["Billing admin"],
    summary = "Edit a promo code (admin).",
    description = "Only `description`, `expires_at`, `max_redemptions` (null = unlimited) and `disabled` can change.",
    params(("id" = Uuid, Path, description = "Promo code UUID")),
    request_body = UpdatePromoCodePayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = PromoCode))
)]
pub async fn update_promo_code(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdatePromoCodePayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    if let Some(Some(description)) = &payload.description
        && description.chars().count() > 255
    {
        return Err(validation(
            "description",
            "Must be at most 255 characters.".into(),
        ));
    }
    if let Some(Some(max)) = payload.max_redemptions
        && !(1..=1_000_000).contains(&max)
    {
        return Err(validation(
            "max_redemptions",
            "Must be between 1 and 1000000.".into(),
        ));
    }
    let promo = state
        .billing_repo
        .update_promo_code(id, &payload)
        .await?
        .ok_or(ApiError::NotFound)?;
    AuditEvent::by(&access, actions::PROMO_UPDATED)
        .target("promo_code", id, &promo.code)
        .meta(json!({ "changes": payload }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(promo))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/promo-codes/{id}/redemptions",
    tags = ["Billing admin"],
    summary = "Who redeemed a promo code (admin).",
    params(("id" = Uuid, Path, description = "Promo code UUID")),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Redemptions.", body = [PromoRedemption]))
)]
pub async fn promo_redemptions(
    State(state): State<AppState>,
    access: AccessControl,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    state
        .billing_repo
        .find_promo_code(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(state.billing_repo.promo_redemptions(id).await?))
}

// ---------------------------------------------------------------------
// Staff: promotions
// ---------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/v1/admin/promotions",
    tags = ["Billing admin"],
    summary = "Promotions (admin).",
    params(PaginationQuery),
    security(("jwt_token" = [])),
    responses((status = 200, description = "Promotions.", body = [Promotion]))
)]
pub async fn list_promotions(
    State(state): State<AppState>,
    access: AccessControl,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    Ok(Json(state.billing_repo.list_promotions().await?))
}

async fn check_promotion_plan(state: &AppState, plan: Option<&str>) -> Result<(), ApiError> {
    if let Some(plan) = plan
        && state.billing_repo.find_plan(plan).await?.is_none()
    {
        return Err(ApiError::rule(
            StatusCode::NOT_FOUND,
            crate::errors::api_error::codes::PLAN_NOT_FOUND,
            "This plan does not exist.",
        ));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/promotions",
    tags = ["Billing admin"],
    summary = "Create a promotion (admin).",
    description = "`plan_code: null` applies to every paid plan. `ends_at` must be after `starts_at`.",
    request_body = CreatePromotionPayload,
    security(("jwt_token" = [])),
    responses((status = 201, description = "Created.", body = Promotion))
)]
pub async fn create_promotion(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Json(payload): Json<CreatePromotionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    if payload.ends_at <= payload.starts_at {
        return Err(validation(
            "ends_at",
            "The end must be after the start.".into(),
        ));
    }
    check_promotion_plan(&state, payload.plan_code.as_deref()).await?;
    let promotion = state
        .billing_repo
        .create_promotion(&payload, access.user_id())
        .await?;
    AuditEvent::by(&access, actions::PROMOTION_CREATED)
        .target("promotion", promotion.id, &promotion.name)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok((StatusCode::CREATED, Json(promotion)))
}

#[utoipa::path(
    patch,
    path = "/api/v1/admin/promotions/{id}",
    tags = ["Billing admin"],
    summary = "Edit a promotion (admin).",
    params(("id" = Uuid, Path, description = "Promotion UUID")),
    request_body = UpdatePromotionPayload,
    security(("jwt_token" = [])),
    responses((status = 200, description = "Updated.", body = Promotion))
)]
pub async fn update_promotion(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdatePromotionPayload>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    payload.validate()?;
    let current = state
        .billing_repo
        .find_promotion(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let starts = payload.starts_at.unwrap_or(current.starts_at);
    let ends = payload.ends_at.unwrap_or(current.ends_at);
    if ends <= starts {
        return Err(validation(
            "ends_at",
            "The end must be after the start.".into(),
        ));
    }
    if let Some(plan) = &payload.plan_code {
        check_promotion_plan(&state, plan.as_deref()).await?;
    }
    let promotion = state
        .billing_repo
        .update_promotion(id, &payload)
        .await?
        .ok_or(ApiError::NotFound)?;
    AuditEvent::by(&access, actions::PROMOTION_UPDATED)
        .target("promotion", id, &promotion.name)
        .meta(json!({ "changes": payload }))
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    Ok(Json(promotion))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/promotions/{id}",
    tags = ["Billing admin"],
    summary = "Delete a promotion (admin).",
    params(("id" = Uuid, Path, description = "Promotion UUID")),
    security(("jwt_token" = [])),
    responses((status = 204, description = "Deleted."))
)]
pub async fn delete_promotion(
    State(state): State<AppState>,
    access: AccessControl,
    ip: ClientIp,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    access.require_admin()?;
    let current = state
        .billing_repo
        .find_promotion(id)
        .await?
        .ok_or(ApiError::NotFound)?;
    state.billing_repo.delete_promotion(id).await?;
    AuditEvent::by(&access, actions::PROMOTION_DELETED)
        .target("promotion", id, &current.name)
        .ip(&ip.0)
        .record(&*state.audit_repo)
        .await;
    info!(promotion_id = %id, "Promotion deleted");
    Ok(StatusCode::NO_CONTENT)
}
