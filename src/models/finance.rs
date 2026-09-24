//! The staff finance report: recurring revenue, subscribers and payments.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Paying subscribers of one plan and interval.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlanRevenue {
    pub plan_code: String,
    /// `monthly` or `yearly`.
    pub interval: String,
    pub subscribers: i64,
    /// Monthly recurring revenue at the price each subscriber is charged
    /// (the plan's list price when unknown), in minor units (a yearly price
    /// counts one twelfth).
    pub mrr_cents: i64,
}

/// What every "net" amount of the report means.
pub const NET_DEFINITION: &str = "gross - refunds - disputes (before Stripe fees and taxes)";

/// Money received in one calendar month (report time zone).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MonthRevenue {
    /// `YYYY-MM`.
    pub month: String,
    pub gross_cents: i64,
    /// Refunds made that month (whatever month the payment was).
    pub refunded_cents: i64,
    /// Payments of that month taken back by a card dispute.
    pub disputed_cents: i64,
    /// Gross − refunds − disputes, before fees (see `net_definition`).
    pub net_cents: i64,
    /// Stripe's fees on that month's payments, where known.
    pub fees_cents: i64,
    pub payments: i64,
    /// Accounts whose first payment was that month.
    pub new_subscribers: i64,
    /// Paid subscriptions that ended that month.
    pub churned: i64,
}

/// One payment, for the recent-payments table.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PaymentRow {
    pub id: Uuid,
    pub invoice_id: String,
    /// Naive UTC.
    pub paid_at: chrono::NaiveDateTime,
    /// `None` once the account was deleted.
    pub user_id: Option<Uuid>,
    pub username: Option<String>,
    pub plan_code: Option<String>,
    pub interval: Option<String>,
    pub amount_cents: i64,
    pub refunded_cents: i64,
    pub disputed_cents: i64,
    /// Stripe's fee, when known.
    pub fee_cents: Option<i64>,
    pub currency: String,
}

/// Received money over a few standard windows (net, see
/// `net_definition`).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct RevenueTotals {
    pub this_month_cents: i64,
    pub last_month_cents: i64,
    pub last_12_months_cents: i64,
    pub all_time_cents: i64,
    pub refunded_this_month_cents: i64,
    pub disputed_this_month_cents: i64,
    /// Stripe's known fees on this month's payments.
    pub fees_this_month_cents: i64,
}

/// Trials that ended in the cohort window and how many became paid.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct TrialConversion {
    /// The window, in days, trials had to end in (ending up to today).
    pub window_days: i64,
    pub ended_trials: i64,
    pub converted: i64,
    /// `converted / ended_trials`, 0 when there were none.
    pub rate: f64,
}

/// `GET /admin/finance`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FinanceOverview {
    /// Whether plans are enforced (the numbers below mean little before).
    pub enforced: bool,
    /// Whether card payments are configured on the server.
    pub payments_enabled: bool,
    /// ISO 4217 of every amount (the plans' currency).
    pub currency: String,
    /// IANA zone months are counted in.
    pub time_zone: String,
    /// What the report's "net" amounts mean.
    pub net_definition: String,
    /// Monthly recurring revenue of paying subscriptions (card-on-file
    /// trials not yet charged are left out).
    pub mrr_cents: i64,
    pub arr_cents: i64,
    /// Average monthly revenue per paying subscriber.
    pub arpu_cents: i64,
    pub paying_subscribers: i64,
    pub by_plan: Vec<PlanRevenue>,
    /// In-app trials running.
    pub trialing: i64,
    /// Paid subscriptions still in their provider-side trial (card on
    /// file, first charge to come).
    pub paid_trialing: i64,
    pub trials_ending_7_days: i64,
    /// Paid subscriptions set to end at the end of their period.
    pub cancel_scheduled: i64,
    pub past_due: i64,
    pub new_subscribers_this_month: i64,
    pub churned_this_month: i64,
    /// Share of paying subscribers at the start of the month that ended.
    pub churn_rate_this_month: f64,
    pub revenue: RevenueTotals,
    pub trial_conversion: TrialConversion,
    /// The last twelve months, oldest first.
    pub monthly: Vec<MonthRevenue>,
    /// The 25 most recent payments.
    pub recent_payments: Vec<PaymentRow>,
    /// Naive UTC of the newest recorded payment.
    pub last_payment_at: Option<chrono::NaiveDateTime>,
}

/// `POST /admin/finance/sync`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct FinanceSyncPayload {
    /// The `cursor` of the previous call, to carry on where it stopped.
    pub cursor: Option<String>,
}

/// `POST /admin/finance/sync`: what this call did. While `done` is false,
/// call again with `cursor`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FinanceSyncResult {
    /// Paid invoices read from the provider.
    pub scanned: i64,
    /// Payments not known before.
    pub imported: i64,
    /// Payments whose refunded amount was brought up to date.
    pub refunds_applied: i64,
    pub done: bool,
    pub cursor: Option<String>,
}
