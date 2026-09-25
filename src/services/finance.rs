//! The staff finance report and the payment ledger behind it.
//!
//! Every payment received is copied into `payments` (see migration 0013_finance):
//! by the Stripe webhook as it happens (`invoice.paid`, `charge.refunded`,
//! `refund.*`, `charge.dispute.*`) and by [`sync_from_provider`], which
//! staff can run to fill in anything the webhook missed (or payments made
//! before the ledger existed). Both are idempotent. Refunds are kept one
//! row each (`refunds`), so they are counted in the month they happened
//! and a refund that later fails stops counting. Recurring revenue is
//! computed from the price each live paid subscription is charged.
//!
//! "Net" everywhere is gross − refunds − disputes, before Stripe's fees
//! (shown separately when known) and taxes.

use crate::{
    database::AppState,
    errors::api_error::ApiError,
    models::finance::{
        FinanceOverview, FinanceSyncResult, MonthRevenue, NET_DEFINITION, PaymentRow, PlanRevenue,
        RevenueTotals, TrialConversion,
    },
    payments::{GatewayInvoice, GatewayRefund, PaymentFees, stripe::parse_refund},
    services::billing,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::FromRow;
use std::collections::BTreeSet;
use tracing::{error, info};
use uuid::Uuid;

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Months are counted in the operator's time zone, so a payment made on
/// the evening of the 31st in Brazil isn't reported in the next month.
pub const REPORT_TIME_ZONE: &str = "America/Sao_Paulo";

/// Trials that ended this many days back make up the conversion cohort.
const CONVERSION_WINDOW_DAYS: i64 = 90;

/// Safety limit for one sync (100 invoices per page).
const MAX_SYNC_PAGES: usize = 200;

const RECENT_PAYMENTS: i64 = 25;

fn unix_to_naive(seconds: i64) -> Option<NaiveDateTime> {
    DateTime::from_timestamp(seconds, 0).map(|d| d.naive_utc())
}

/// The account a paid invoice belongs to: by Stripe customer first, then
/// by the account id recorded on the subscription.
async fn account_of(state: &AppState, invoice: &GatewayInvoice) -> Result<Option<Uuid>, ApiError> {
    if let Some(customer_id) = &invoice.customer_id {
        let found: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE stripe_customer_id = $1")
                .bind(customer_id)
                .fetch_optional(&state.db)
                .await?;
        if found.is_some() {
            return Ok(found);
        }
    }
    let Some(user_id) = invoice.user_id else {
        return Ok(None);
    };
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?)
}

/// Records a paid invoice in the ledger. `true` when it wasn't known yet.
pub async fn record_invoice(state: &AppState, invoice: &GatewayInvoice) -> Result<bool, ApiError> {
    if invoice.amount_paid <= 0 {
        return Ok(false);
    }
    let user_id = account_of(state, invoice).await?;
    let paid_at = invoice.paid_at.and_then(unix_to_naive).unwrap_or_else(now);
    let timestamp = now();
    let inserted: bool = sqlx::query_scalar(
        "INSERT INTO payments (id, provider, invoice_id, payment_intent_id, customer_id,
                               subscription_id, user_id, plan_code, billing_interval,
                               amount_cents, currency, paid_at, created_at, updated_at)
         VALUES ($1, 'stripe', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)
         ON CONFLICT (provider, invoice_id) DO UPDATE SET
             payment_intent_id = COALESCE(payments.payment_intent_id, EXCLUDED.payment_intent_id),
             customer_id = COALESCE(payments.customer_id, EXCLUDED.customer_id),
             subscription_id = COALESCE(payments.subscription_id, EXCLUDED.subscription_id),
             user_id = COALESCE(payments.user_id, EXCLUDED.user_id),
             plan_code = COALESCE(payments.plan_code, EXCLUDED.plan_code),
             billing_interval = COALESCE(payments.billing_interval, EXCLUDED.billing_interval),
             updated_at = EXCLUDED.updated_at
         RETURNING (xmax = 0)",
    )
    .bind(Uuid::now_v7())
    .bind(&invoice.id)
    .bind(&invoice.payment_intent_id)
    .bind(&invoice.customer_id)
    .bind(&invoice.subscription_id)
    .bind(user_id)
    .bind(&invoice.plan_code)
    .bind(invoice.interval.map(|i| i.key()))
    .bind(invoice.amount_paid)
    .bind(invoice.currency.to_ascii_uppercase())
    .bind(paid_at)
    .bind(timestamp)
    .fetch_one(&state.db)
    .await?;
    if inserted {
        info!(invoice = %invoice.id, amount = invoice.amount_paid, "Payment recorded");
        // The account's first payment is what earns its referrer a reward.
        if let Some(user_id) = user_id
            && let Err(e) = billing::reward_referrer_on_payment(state, user_id).await
        {
            error!(%user_id, error = %e, "Could not pay out a referral");
        }
    }
    Ok(inserted)
}

/// Stores Stripe's fee and the net amount of a payment.
pub async fn set_fees(
    state: &AppState,
    invoice_id: &str,
    fees: &PaymentFees,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE payments SET fee_cents = $2, net_cents = $3,
                             charge_id = COALESCE(charge_id, $4), updated_at = $5
         WHERE provider = 'stripe' AND invoice_id = $1",
    )
    .bind(invoice_id)
    .bind(fees.fee)
    .bind(fees.net)
    .bind(&fees.charge_id)
    .bind(now())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Sets a payment's cumulative refunded amount from a `charge.refunded`
/// event: never below what is already known (replays and out-of-order
/// deliveries are harmless), nor below its recorded refunds. Rows updated.
async fn set_refunded(
    state: &AppState,
    payment_intent: &str,
    refunded: i64,
) -> Result<u64, ApiError> {
    Ok(sqlx::query(
        "UPDATE payments
         SET refunded_cents = LEAST(amount_cents, GREATEST(refunded_cents, $2,
                 (SELECT COALESCE(SUM(amount_cents), 0) FROM refunds
                  WHERE payment_intent_id = $1 AND status IN ('succeeded', 'pending')))),
             updated_at = $3
         WHERE payment_intent_id = $1",
    )
    .bind(payment_intent)
    .bind(refunded.max(0))
    .bind(now())
    .execute(&state.db)
    .await?
    .rows_affected())
}

/// Derives a payment's refunded amount from its refunds that count
/// (`succeeded` or `pending`): a refund that failed stops counting. Rows
/// updated.
async fn recompute_refunded(state: &AppState, payment_intent: &str) -> Result<u64, ApiError> {
    Ok(sqlx::query(
        "UPDATE payments
         SET refunded_cents = LEAST(amount_cents,
                 (SELECT COALESCE(SUM(amount_cents), 0) FROM refunds
                  WHERE payment_intent_id = $1 AND status IN ('succeeded', 'pending'))),
             updated_at = $2
         WHERE payment_intent_id = $1",
    )
    .bind(payment_intent)
    .bind(now())
    .execute(&state.db)
    .await?
    .rows_affected())
}

/// Stores (or updates) one refund.
async fn store_refund(state: &AppState, refund: &GatewayRefund) -> Result<(), ApiError> {
    let created_at = refund.created.and_then(unix_to_naive).unwrap_or_else(now);
    sqlx::query(
        "INSERT INTO refunds (id, payment_intent_id, charge_id, amount_cents, currency, status,
                              created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (id) DO UPDATE SET
             payment_intent_id = COALESCE(refunds.payment_intent_id, EXCLUDED.payment_intent_id),
             charge_id = COALESCE(refunds.charge_id, EXCLUDED.charge_id),
             amount_cents = EXCLUDED.amount_cents, status = EXCLUDED.status,
             updated_at = EXCLUDED.updated_at",
    )
    .bind(&refund.id)
    .bind(&refund.payment_intent_id)
    .bind(&refund.charge_id)
    .bind(refund.amount.max(0))
    .bind(&refund.currency)
    .bind(&refund.status)
    .bind(created_at)
    .bind(now())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Records a refund and brings its payment's refunded amount up to date.
/// A refund of a payment the ledger doesn't have yet brings that invoice
/// in first.
pub async fn upsert_refund(state: &AppState, refund: &GatewayRefund) -> Result<(), ApiError> {
    store_refund(state, refund).await?;
    let Some(payment_intent) = &refund.payment_intent_id else {
        return Ok(());
    };
    if recompute_refunded(state, payment_intent).await? == 0
        && import_invoice_of(state, payment_intent).await?
    {
        recompute_refunded(state, payment_intent).await?;
    }
    Ok(())
}

/// Applies a `refund.created`, `refund.updated` or `charge.refund.updated`
/// event object.
pub async fn apply_refund_object(state: &AppState, object: &Value) -> Result<(), ApiError> {
    match parse_refund(object) {
        Some(refund) => upsert_refund(state, &refund).await,
        None => Ok(()),
    }
}

/// Brings the paid invoice settled by `payment_intent` into the ledger,
/// when the provider knows one. `true` when the ledger now has it.
async fn import_invoice_of(state: &AppState, payment_intent: &str) -> Result<bool, ApiError> {
    let Some(payments) = state.payments.as_ref() else {
        return Ok(false);
    };
    let Some(invoice_id) = payments.gateway.invoice_for_payment(payment_intent).await? else {
        return Ok(false);
    };
    let Some(invoice) = payments.gateway.get_paid_invoice(&invoice_id).await? else {
        return Ok(false);
    };
    record_invoice(state, &invoice).await?;
    Ok(true)
}

fn id_of(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(id)) => Some(id.as_str()),
        Some(object) => object.get("id").and_then(Value::as_str),
        None => None,
    }
}

/// Applies a `charge.refunded` event's charge: its refunds are recorded
/// one by one (from the provider, with their own dates) and the charge's
/// cumulative refunded amount goes on the payment it settled. A refund for
/// a payment the ledger doesn't have yet (its `invoice.paid` still on the
/// way, or a payment older than the ledger) brings that invoice in first;
/// a charge that settled no invoice is not a subscription payment and is
/// ignored. Returns the payment (intent) when the ledger has it.
pub async fn apply_refund_event(
    state: &AppState,
    charge: &Value,
) -> Result<Option<String>, ApiError> {
    let (Some(payment_intent), Some(refunded)) = (
        id_of(charge.get("payment_intent")),
        charge.get("amount_refunded").and_then(Value::as_i64),
    ) else {
        return Ok(None);
    };
    if let Some(payments) = state.payments.as_ref() {
        match payments
            .gateway
            .list_refunds_for_payment(payment_intent)
            .await
        {
            Ok(refunds) => {
                for refund in &refunds {
                    store_refund(state, refund).await?;
                }
            }
            // The cumulative amount below still applies.
            Err(e) => error!(payment_intent, error = %e, "Could not list a charge's refunds"),
        }
    }
    if set_refunded(state, payment_intent, refunded).await? > 0 {
        return Ok(Some(payment_intent.to_string()));
    }
    if import_invoice_of(state, payment_intent).await? {
        set_refunded(state, payment_intent, refunded).await?;
        return Ok(Some(payment_intent.to_string()));
    }
    Ok(None)
}

/// `(account, subscription)` when `payment_intent` was the latest payment
/// of its subscription and has been refunded in full.
pub async fn fully_refunded_latest_payment(
    state: &AppState,
    payment_intent: &str,
) -> Result<Option<(Uuid, String)>, ApiError> {
    Ok(sqlx::query_as(
        "SELECT p.user_id, p.subscription_id FROM payments p
         WHERE p.payment_intent_id = $1 AND p.user_id IS NOT NULL
           AND p.subscription_id IS NOT NULL AND p.refunded_cents >= p.amount_cents
           AND NOT EXISTS (SELECT 1 FROM payments q
                           WHERE q.subscription_id = p.subscription_id AND q.paid_at > p.paid_at)
         LIMIT 1",
    )
    .bind(payment_intent)
    .fetch_optional(&state.db)
    .await?)
}

/// A disputed payment, as the ledger now has it.
#[derive(Debug, Clone)]
pub struct DisputedPayment {
    pub user_id: Option<Uuid>,
    pub subscription_id: Option<String>,
    /// Minor units disputed (0 once the dispute was won).
    pub amount: i64,
    /// Stripe's dispute status (`needs_response`, `won`, `lost`...).
    pub status: String,
}

async fn mark_disputed(
    state: &AppState,
    payment_intent: &str,
    amount: i64,
    status: &str,
    charge: Option<&str>,
) -> Result<Option<(Option<Uuid>, Option<String>)>, ApiError> {
    Ok(sqlx::query_as(
        "UPDATE payments
         SET disputed_cents = LEAST(amount_cents, $2), dispute_status = $3,
             charge_id = COALESCE(charge_id, $4), updated_at = $5
         WHERE payment_intent_id = $1
         RETURNING user_id, subscription_id",
    )
    .bind(payment_intent)
    .bind(amount.max(0))
    .bind(status)
    .bind(charge)
    .bind(now())
    .fetch_optional(&state.db)
    .await?)
}

/// Applies a `charge.dispute.*` event's dispute to the payment it
/// contests: the disputed amount counts against revenue unless the dispute
/// was won. `None` when the payment isn't a subscription payment.
pub async fn apply_dispute(
    state: &AppState,
    dispute: &Value,
) -> Result<Option<DisputedPayment>, ApiError> {
    let Some(payment_intent) = id_of(dispute.get("payment_intent")) else {
        return Ok(None);
    };
    let status = dispute
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("needs_response")
        .to_string();
    let amount = if status == "won" {
        0
    } else {
        dispute.get("amount").and_then(Value::as_i64).unwrap_or(0)
    };
    let charge = id_of(dispute.get("charge"));
    let mut row = mark_disputed(state, payment_intent, amount, &status, charge).await?;
    if row.is_none() && import_invoice_of(state, payment_intent).await? {
        row = mark_disputed(state, payment_intent, amount, &status, charge).await?;
    }
    Ok(row.map(|(user_id, subscription_id)| DisputedPayment {
        user_id,
        subscription_id,
        amount,
        status,
    }))
}

/// How long one sync call may keep reading from the provider before it
/// hands back a cursor: well under the API's request timeout and the web
/// server's function limit. The caller repeats with the cursor.
const SYNC_TIME_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// Where a sync stopped: reading invoices or refunds, after an id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncCursor {
    Invoices(Option<String>),
    Refunds(Option<String>),
}

impl SyncCursor {
    fn parse(value: Option<&str>) -> Self {
        let value = value.map(str::trim).unwrap_or_default();
        let after = |rest: &str| Some(rest.to_string()).filter(|r| !r.is_empty());
        if let Some(rest) = value.strip_prefix("refunds:") {
            SyncCursor::Refunds(after(rest))
        } else if let Some(rest) = value.strip_prefix("invoices:") {
            SyncCursor::Invoices(after(rest))
        } else {
            SyncCursor::Invoices(None)
        }
    }

    fn encode(&self) -> String {
        match self {
            SyncCursor::Invoices(after) => format!("invoices:{}", after.as_deref().unwrap_or("")),
            SyncCursor::Refunds(after) => format!("refunds:{}", after.as_deref().unwrap_or("")),
        }
    }
}

/// Reads paid invoices and then refunds from the provider, recording the
/// missing payments and refunded amounts. Each call works for at most
/// [`SYNC_TIME_BUDGET`]; while `done` is false, call again with `cursor`.
pub async fn sync_from_provider(
    state: &AppState,
    cursor: Option<&str>,
) -> Result<FinanceSyncResult, ApiError> {
    let payments = state.payments.as_ref().ok_or_else(|| {
        ApiError::rule(
            axum::http::StatusCode::CONFLICT,
            crate::errors::api_error::codes::PAYMENTS_UNAVAILABLE,
            "Card payments are not configured on the server.",
        )
    })?;
    let started = std::time::Instant::now();
    let mut result = FinanceSyncResult {
        scanned: 0,
        imported: 0,
        refunds_applied: 0,
        done: false,
        cursor: None,
    };
    let mut position = SyncCursor::parse(cursor);

    for _ in 0..MAX_SYNC_PAGES {
        if started.elapsed() >= SYNC_TIME_BUDGET {
            result.cursor = Some(position.encode());
            return Ok(result);
        }
        match &position {
            SyncCursor::Invoices(after) => {
                let page = payments
                    .gateway
                    .list_paid_invoices(after.as_deref())
                    .await?;
                for invoice in &page.invoices {
                    result.scanned += 1;
                    if record_invoice(state, invoice).await? {
                        result.imported += 1;
                    }
                }
                position = match (page.has_more, page.last_id) {
                    (true, Some(last)) => SyncCursor::Invoices(Some(last)),
                    _ => SyncCursor::Refunds(None),
                };
            }
            SyncCursor::Refunds(after) => {
                let page = payments.gateway.list_refunds(after.as_deref()).await?;
                // Every refund is stored (whatever its status), then each
                // payment's total is derived from all its stored refunds,
                // so refunds spread over several pages add up.
                let mut touched = BTreeSet::new();
                for refund in &page.refunds {
                    store_refund(state, refund).await?;
                    if let Some(payment_intent) = &refund.payment_intent_id {
                        touched.insert(payment_intent.clone());
                    }
                }
                for payment_intent in &touched {
                    result.refunds_applied +=
                        recompute_refunded(state, payment_intent).await? as i64;
                }
                match (page.has_more, page.last_id) {
                    (true, Some(last)) => position = SyncCursor::Refunds(Some(last)),
                    _ => {
                        result.done = true;
                        info!(
                            scanned = result.scanned,
                            imported = result.imported,
                            "Finance sync finished"
                        );
                        return Ok(result);
                    }
                }
            }
        }
    }
    result.cursor = Some(position.encode());
    Ok(result)
}

#[derive(FromRow)]
struct MonthRow {
    month: String,
    gross: i64,
    refunded: i64,
    disputed: i64,
    fees: i64,
    payments: i64,
    new_subscribers: i64,
    churned: i64,
}

#[derive(FromRow)]
struct StatusCounts {
    trialing: i64,
    paid_trialing: i64,
    trials_ending: i64,
    cancel_scheduled: i64,
    past_due: i64,
}

#[derive(FromRow)]
struct RecentRow {
    id: Uuid,
    invoice_id: String,
    paid_at: NaiveDateTime,
    user_id: Option<Uuid>,
    username: Option<String>,
    plan_code: Option<String>,
    billing_interval: Option<String>,
    amount_cents: i64,
    refunded_cents: i64,
    disputed_cents: i64,
    fee_cents: Option<i64>,
    currency: String,
}

fn ratio(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        0.0
    } else {
        (part as f64 / whole as f64).clamp(0.0, 1.0)
    }
}

/// Builds the finance report as of `at` (naive UTC).
pub async fn overview_at(state: &AppState, at: NaiveDateTime) -> Result<FinanceOverview, ApiError> {
    let settings = state.billing_repo.get_settings().await?;
    let currency: String = sqlx::query_scalar(
        "SELECT UPPER(currency) FROM plans ORDER BY is_public DESC, sort_order, code LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_else(|| "BRL".to_string());

    // Paying subscribers at the price each one is charged; card-on-file
    // trials haven't paid anything yet and are left out.
    let by_plan: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT s.plan_code,
                COALESCE(s.billing_interval, 'monthly'),
                COUNT(*)::BIGINT,
                COALESCE(SUM(CASE WHEN s.billing_interval = 'yearly'
                                  THEN ROUND(COALESCE(s.unit_amount_cents, p.price_yearly_cents) / 12.0)
                                  ELSE COALESCE(s.unit_amount_cents, p.price_monthly_cents) END), 0)::BIGINT
         FROM subscriptions s
         JOIN plans p ON p.code = s.plan_code
         WHERE s.source = 'payment' AND s.status IN ('active', 'past_due')
           AND COALESCE(s.provider_status, 'active') <> 'trialing'
         GROUP BY 1, 2
         ORDER BY 4 DESC, 1, 2",
    )
    .fetch_all(&state.db)
    .await?;
    let by_plan: Vec<PlanRevenue> = by_plan
        .into_iter()
        .map(
            |(plan_code, interval, subscribers, mrr_cents)| PlanRevenue {
                plan_code,
                interval,
                subscribers,
                mrr_cents,
            },
        )
        .collect();
    let mrr_cents: i64 = by_plan.iter().map(|p| p.mrr_cents).sum();
    let paying_subscribers: i64 = by_plan.iter().map(|p| p.subscribers).sum();

    let counts: StatusCounts = sqlx::query_as(
        "SELECT
             COUNT(*) FILTER (WHERE status = 'trialing')::BIGINT AS trialing,
             COUNT(*) FILTER (WHERE source = 'payment' AND status = 'active'
                              AND provider_status = 'trialing')::BIGINT AS paid_trialing,
             COUNT(*) FILTER (WHERE status = 'trialing'
                              AND COALESCE(trial_ends_at, current_period_end) > $1
                              AND COALESCE(trial_ends_at, current_period_end) <= $1 + INTERVAL '7 days'
                             )::BIGINT AS trials_ending,
             COUNT(*) FILTER (WHERE source = 'payment' AND status IN ('active', 'past_due')
                              AND cancel_at_period_end)::BIGINT AS cancel_scheduled,
             COUNT(*) FILTER (WHERE source = 'payment' AND status = 'past_due')::BIGINT AS past_due
         FROM subscriptions",
    )
    .bind(at)
    .fetch_one(&state.db)
    .await?;

    // The last twelve months in the report time zone: money received,
    // refunds by the month they were made (a refunded amount known only
    // from its charge falls in the payment's month), disputes, known fees,
    // first payments and paid subscriptions that ended.
    let monthly: Vec<MonthRow> = sqlx::query_as(
        "WITH local_now AS (
             SELECT ($1::timestamp AT TIME ZONE 'UTC') AT TIME ZONE $2 AS t
         ),
         months AS (
             SELECT generate_series(
                        date_trunc('month', (SELECT t FROM local_now)) - INTERVAL '11 months',
                        date_trunc('month', (SELECT t FROM local_now)),
                        INTERVAL '1 month') AS m
         ),
         paid AS (
             SELECT date_trunc('month', (paid_at AT TIME ZONE 'UTC') AT TIME ZONE $2) AS m,
                    SUM(amount_cents) AS gross,
                    SUM(disputed_cents) AS disputed,
                    SUM(COALESCE(fee_cents, 0)) AS fees,
                    COUNT(*) AS payments
             FROM payments
             GROUP BY 1
         ),
         counted AS (
             SELECT r.payment_intent_id, r.amount_cents, r.created_at
             FROM refunds r
             WHERE r.status IN ('succeeded', 'pending')
               AND EXISTS (SELECT 1 FROM payments p WHERE p.payment_intent_id = r.payment_intent_id)
         ),
         refunded AS (
             SELECT m, SUM(amount) AS refunded FROM (
                 SELECT date_trunc('month', (created_at AT TIME ZONE 'UTC') AT TIME ZONE $2) AS m,
                        amount_cents AS amount
                 FROM counted
                 UNION ALL
                 SELECT date_trunc('month', (p.paid_at AT TIME ZONE 'UTC') AT TIME ZONE $2),
                        GREATEST(p.refunded_cents - COALESCE(
                            (SELECT SUM(c.amount_cents) FROM counted c
                             WHERE c.payment_intent_id = p.payment_intent_id), 0), 0)
                 FROM payments p
             ) x
             GROUP BY 1
         ),
         firsts AS (
             SELECT date_trunc('month', (MIN(paid_at) AT TIME ZONE 'UTC') AT TIME ZONE $2) AS m
             FROM payments
             WHERE user_id IS NOT NULL AND amount_cents > 0
             GROUP BY user_id
         ),
         new_subscribers AS (
             SELECT m, COUNT(*) AS new_subscribers FROM firsts GROUP BY 1
         ),
         changes AS (
             SELECT date_trunc('month', (created_at AT TIME ZONE 'UTC') AT TIME ZONE $2) AS m,
                    COUNT(*) AS churned
             FROM subscription_events
             WHERE kind IN ('canceled', 'withdrawn', 'disputed', 'refunded', 'expired')
               AND data->>'source' = 'payment'
             GROUP BY 1
         )
         SELECT to_char(months.m, 'YYYY-MM') AS month,
                COALESCE(paid.gross, 0)::BIGINT AS gross,
                COALESCE(refunded.refunded, 0)::BIGINT AS refunded,
                COALESCE(paid.disputed, 0)::BIGINT AS disputed,
                COALESCE(paid.fees, 0)::BIGINT AS fees,
                COALESCE(paid.payments, 0)::BIGINT AS payments,
                COALESCE(new_subscribers.new_subscribers, 0)::BIGINT AS new_subscribers,
                COALESCE(changes.churned, 0)::BIGINT AS churned
         FROM months
         LEFT JOIN paid ON paid.m = months.m
         LEFT JOIN refunded ON refunded.m = months.m
         LEFT JOIN new_subscribers ON new_subscribers.m = months.m
         LEFT JOIN changes ON changes.m = months.m
         ORDER BY months.m",
    )
    .bind(at)
    .bind(REPORT_TIME_ZONE)
    .fetch_all(&state.db)
    .await?;
    let monthly: Vec<MonthRevenue> = monthly
        .into_iter()
        .map(|m| MonthRevenue {
            month: m.month,
            gross_cents: m.gross,
            refunded_cents: m.refunded,
            disputed_cents: m.disputed,
            net_cents: m.gross - m.refunded - m.disputed,
            fees_cents: m.fees,
            payments: m.payments,
            new_subscribers: m.new_subscribers,
            churned: m.churned,
        })
        .collect();

    let (all_time, last_payment_at): (i64, Option<NaiveDateTime>) = sqlx::query_as(
        "SELECT COALESCE(SUM(amount_cents - refunded_cents - disputed_cents), 0)::BIGINT, MAX(paid_at)
         FROM payments",
    )
    .fetch_one(&state.db)
    .await?;

    let current = monthly.last();
    let previous = monthly.len().checked_sub(2).and_then(|i| monthly.get(i));
    let revenue = RevenueTotals {
        this_month_cents: current.map_or(0, |m| m.net_cents),
        last_month_cents: previous.map_or(0, |m| m.net_cents),
        last_12_months_cents: monthly.iter().map(|m| m.net_cents).sum(),
        all_time_cents: all_time,
        refunded_this_month_cents: current.map_or(0, |m| m.refunded_cents),
        disputed_this_month_cents: current.map_or(0, |m| m.disputed_cents),
        fees_this_month_cents: current.map_or(0, |m| m.fees_cents),
    };
    let new_subscribers_this_month = current.map_or(0, |m| m.new_subscribers);
    let churned_this_month = current.map_or(0, |m| m.churned);
    // Subscribers at the start of the month ≈ now − joined + left.
    let at_month_start = paying_subscribers - new_subscribers_this_month + churned_this_month;

    // A trial converts when the account pays its first invoice (not when
    // it merely starts a subscription whose own trial may still be
    // canceled before the first charge).
    let (ended_trials, converted): (i64, i64) = sqlx::query_as(
        "WITH trials AS (
             -- When each account's trial ended: its start plus every day
             -- granted, extensions included.
             SELECT e.user_id,
                    MIN(e.created_at) FILTER (WHERE e.kind = 'trial_started') AS started,
                    SUM(COALESCE((e.data->>'days')::INT, 0))::INT AS days
             FROM subscription_events e
             WHERE e.kind IN ('trial_started', 'trial_extended')
             GROUP BY e.user_id
         ),
         ended AS (
             SELECT user_id FROM trials
             WHERE started IS NOT NULL
               AND started + make_interval(days => days)
                   BETWEEN $1 - make_interval(days => $2::INT) AND $1
         )
         SELECT COUNT(*)::BIGINT,
                COUNT(*) FILTER (WHERE EXISTS (
                    SELECT 1 FROM payments p
                    WHERE p.user_id = ended.user_id AND p.amount_cents > 0))::BIGINT
         FROM ended",
    )
    .bind(at)
    .bind(CONVERSION_WINDOW_DAYS as i32)
    .fetch_one(&state.db)
    .await?;

    let recent: Vec<RecentRow> = sqlx::query_as(
        "SELECT p.id, p.invoice_id, p.paid_at, p.user_id, u.username, p.plan_code,
                p.billing_interval, p.amount_cents, p.refunded_cents, p.disputed_cents,
                p.fee_cents, p.currency
         FROM payments p
         LEFT JOIN users u ON u.id = p.user_id
         ORDER BY p.paid_at DESC, p.id DESC
         LIMIT $1",
    )
    .bind(RECENT_PAYMENTS)
    .fetch_all(&state.db)
    .await?;

    Ok(FinanceOverview {
        enforced: settings.enforced,
        payments_enabled: state.payments.is_some(),
        currency,
        time_zone: REPORT_TIME_ZONE.to_string(),
        net_definition: NET_DEFINITION.to_string(),
        mrr_cents,
        arr_cents: mrr_cents * 12,
        arpu_cents: if paying_subscribers > 0 {
            mrr_cents / paying_subscribers
        } else {
            0
        },
        paying_subscribers,
        by_plan,
        trialing: counts.trialing,
        paid_trialing: counts.paid_trialing,
        trials_ending_7_days: counts.trials_ending,
        cancel_scheduled: counts.cancel_scheduled,
        past_due: counts.past_due,
        new_subscribers_this_month,
        churned_this_month,
        churn_rate_this_month: ratio(churned_this_month, at_month_start),
        revenue,
        trial_conversion: TrialConversion {
            window_days: CONVERSION_WINDOW_DAYS,
            ended_trials,
            converted,
            rate: ratio(converted, ended_trials),
        },
        monthly,
        recent_payments: recent
            .into_iter()
            .map(|r| PaymentRow {
                id: r.id,
                invoice_id: r.invoice_id,
                paid_at: r.paid_at,
                user_id: r.user_id,
                username: r.username,
                plan_code: r.plan_code,
                interval: r.billing_interval,
                amount_cents: r.amount_cents,
                refunded_cents: r.refunded_cents,
                disputed_cents: r.disputed_cents,
                fee_cents: r.fee_cents,
                currency: r.currency,
            })
            .collect(),
        last_payment_at,
    })
}

pub async fn overview(state: &AppState) -> Result<FinanceOverview, ApiError> {
    overview_at(state, now()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_are_bounded_and_safe() {
        assert_eq!(ratio(1, 0), 0.0);
        assert_eq!(ratio(1, 4), 0.25);
        assert_eq!(ratio(5, 4), 1.0);
        assert_eq!(ratio(-1, 4), 0.0);
    }

    #[test]
    fn sync_cursors_round_trip() {
        for cursor in [
            SyncCursor::Invoices(None),
            SyncCursor::Invoices(Some("in_1".into())),
            SyncCursor::Refunds(None),
            SyncCursor::Refunds(Some("re_1".into())),
        ] {
            assert_eq!(SyncCursor::parse(Some(&cursor.encode())), cursor);
        }
        assert_eq!(SyncCursor::parse(None), SyncCursor::Invoices(None));
        assert_eq!(SyncCursor::parse(Some("junk")), SyncCursor::Invoices(None));
    }

    #[test]
    fn unix_times_convert() {
        assert_eq!(
            unix_to_naive(0).unwrap(),
            DateTime::from_timestamp(0, 0).unwrap().naive_utc()
        );
    }
}
