//! Plans, subscriptions, promo codes, promotions, referrals and credits.

use crate::{
    models::{quota::QuotaLimits, user_preferences::SUPPORTED_LANGUAGES},
    services::entitlements::Feature,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Type};
use std::{borrow::Cow, collections::BTreeMap};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::{Validate, ValidationError};

/// Platform-setting key of [`BillingSettings`].
pub const BILLING_SETTINGS_KEY: &str = "billing";

fn error(code: &'static str, message: impl Into<String>) -> ValidationError {
    let mut error = ValidationError::new(code);
    error.message = Some(Cow::from(message.into()));
    error
}

/// A localized text map `{"en": ..., "pt-BR": ..., "es": ...}`: only
/// supported locales, `required` ones present, every value `min..=max`
/// characters after trimming.
pub fn validate_localized_map(
    value: &Value,
    required: &[&str],
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    let Some(map) = value.as_object() else {
        return Err(error(
            "invalid_localized_text",
            "Must be an object keyed by locale.",
        ));
    };
    for (locale, text) in map {
        if !SUPPORTED_LANGUAGES.contains(&locale.as_str()) {
            return Err(error(
                "invalid_localized_text",
                format!("Unsupported locale '{locale}'."),
            ));
        }
        let Some(text) = text.as_str() else {
            return Err(error("invalid_localized_text", "Values must be strings."));
        };
        let len = text.trim().chars().count();
        if len < min || len > max {
            return Err(error(
                "invalid_localized_text",
                format!("Each text must have between {min} and {max} characters."),
            ));
        }
    }
    for locale in required {
        if !map.contains_key(*locale) {
            return Err(error(
                "invalid_localized_text",
                format!("The '{locale}' text is required."),
            ));
        }
    }
    Ok(())
}

/// Trims every value of a localized map (after validation).
pub fn trim_localized(value: &Value) -> Value {
    match value.as_object() {
        Some(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        Value::String(v.as_str().unwrap_or_default().trim().to_string()),
                    )
                })
                .collect(),
        ),
        None => value.clone(),
    }
}

// ---------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct ReferralSettings {
    pub enabled: bool,
    #[validate(range(min = 0, max = 100_000))]
    pub referrer_credits: i32,
    #[validate(range(min = 0, max = 100_000))]
    pub referred_credits: i32,
    /// Rewarded referrals per referrer per calendar month.
    #[validate(range(min = 0, max = 10_000))]
    pub max_rewarded_per_month: i64,
}

impl Default for ReferralSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            referrer_credits: 50,
            referred_credits: 20,
            max_rewarded_per_month: 20,
        }
    }
}

/// Plan time that can be bought with credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Validate)]
pub struct Reward {
    #[validate(length(min = 1, max = 40))]
    pub id: String,
    #[validate(length(min = 1, max = 32))]
    pub plan: String,
    #[validate(range(min = 1, max = 3650))]
    pub days: i64,
    #[validate(range(min = 1, max = 100_000))]
    pub cost: i32,
}

/// Platform billing configuration (`platform_settings.billing`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, Validate)]
#[serde(default)]
pub struct BillingSettings {
    /// While `false`, every account has every feature and the platform
    /// default quotas apply.
    pub enforced: bool,
    #[validate(range(min = 0, max = 365))]
    pub trial_days: i64,
    #[validate(length(min = 1, max = 32))]
    pub trial_plan: String,
    #[validate(nested)]
    pub referral: ReferralSettings,
    #[validate(nested, length(max = 20))]
    pub rewards: Vec<Reward>,
}

impl Default for BillingSettings {
    fn default() -> Self {
        Self {
            enforced: false,
            trial_days: 30,
            trial_plan: "pro".into(),
            referral: ReferralSettings::default(),
            rewards: vec![
                Reward {
                    id: "intermediate_30d".into(),
                    plan: "intermediate".into(),
                    days: 30,
                    cost: 80,
                },
                Reward {
                    id: "pro_30d".into(),
                    plan: "pro".into(),
                    days: 30,
                    cost: 120,
                },
            ],
        }
    }
}

/// Which rules an account falls under right now, in order of precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessTier {
    /// Admins and moderators: every feature, no quotas, no plans to buy.
    Staff,
    /// A plan in effect while plans are enforced: its features and limits.
    Plan,
    /// E-mail address not verified yet (and no plan): very small limits
    /// and no paid features, in the beta too.
    Unverified,
    /// Plans not enforced (the beta): every feature, the platform defaults.
    Beta,
    /// Plans enforced and no plan in effect: the free tier.
    Free,
}

impl AccessTier {
    pub fn resolve(is_staff: bool, enforced: bool, has_plan: bool, email_verified: bool) -> Self {
        if is_staff {
            AccessTier::Staff
        } else if enforced && has_plan {
            AccessTier::Plan
        } else if !email_verified {
            AccessTier::Unverified
        } else if !enforced {
            AccessTier::Beta
        } else {
            AccessTier::Free
        }
    }
}

/// Answer of `GET /public/billing`: whether the platform is in its beta
/// (plans not enforced, everything free) and the sign-up trial.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicBillingMode {
    /// `true` while plans aren't enforced: every feature is free.
    pub beta: bool,
    /// Days of the sign-up trial (starts once the e-mail is verified; only
    /// once plans are enforced).
    pub trial_days: i64,
    pub trial_plan: String,
    /// Limits of every verified account during the beta.
    pub beta_limits: QuotaLimits,
    /// The free tier (no plan, once plans are enforced).
    pub free_limits: QuotaLimits,
    pub free_features: BTreeMap<String, bool>,
    /// Limits until the e-mail address is verified.
    pub unverified_limits: QuotaLimits,
}

// ---------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------

/// The flags of every known feature, missing keys as `false`.
pub fn normalize_features(stored: &Value) -> BTreeMap<String, bool> {
    Feature::ALL
        .iter()
        .map(|f| {
            (
                f.key().to_string(),
                stored
                    .get(f.key())
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
        })
        .collect()
}

#[derive(Debug, Clone, FromRow)]
pub struct PlanRow {
    pub code: String,
    pub name: Value,
    pub description: Value,
    pub price_monthly_cents: i32,
    pub price_yearly_cents: i32,
    pub currency: String,
    pub limits: Value,
    pub features: Value,
    pub highlighted: bool,
    pub is_public: bool,
    pub sort_order: i32,
    pub updated_at: NaiveDateTime,
}

/// A plan.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Plan {
    pub code: String,
    /// Localized: `{"en": ..., "pt-BR": ..., "es": ...}`.
    pub name: Value,
    pub description: Value,
    pub price_monthly_cents: i32,
    pub price_yearly_cents: i32,
    pub currency: String,
    pub limits: QuotaLimits,
    /// Every known feature, `true` when included.
    pub features: BTreeMap<String, bool>,
    pub highlighted: bool,
    pub is_public: bool,
    pub sort_order: i32,
    pub updated_at: NaiveDateTime,
}

impl From<PlanRow> for Plan {
    fn from(row: PlanRow) -> Self {
        Self {
            code: row.code,
            name: row.name,
            description: row.description,
            price_monthly_cents: row.price_monthly_cents,
            price_yearly_cents: row.price_yearly_cents,
            currency: row.currency,
            limits: serde_json::from_value(row.limits).unwrap_or_default(),
            features: normalize_features(&row.features),
            highlighted: row.highlighted,
            is_public: row.is_public,
            sort_order: row.sort_order,
            updated_at: row.updated_at,
        }
    }
}

impl Plan {
    pub fn has(&self, feature: Feature) -> bool {
        self.features.get(feature.key()).copied().unwrap_or(false)
    }
}

/// The promotion shown next to a plan on the pricing page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, FromRow)]
pub struct PlanPromotion {
    pub id: Uuid,
    /// Localized headline map.
    pub headline: Value,
    pub discount_percent: i32,
    pub ends_at: NaiveDateTime,
}

/// A plan as listed publicly (`GET /public/plans`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicPlan {
    pub code: String,
    pub name: Value,
    pub description: Value,
    pub price_monthly_cents: i32,
    pub price_yearly_cents: i32,
    pub currency: String,
    pub limits: QuotaLimits,
    pub features: BTreeMap<String, bool>,
    pub highlighted: bool,
    pub sort_order: i32,
    /// The best active promotion for this plan, if any.
    pub promotion: Option<PlanPromotion>,
}

fn validate_plan_code(code: &str) -> Result<(), ValidationError> {
    let valid = (2..=32).contains(&code.len())
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !matches!(code, "none" | "trial");
    if valid {
        Ok(())
    } else {
        Err(error(
            "invalid_plan_code",
            "Plan codes use 2 to 32 lower-case letters, digits and underscores ('none' and 'trial' are reserved).",
        ))
    }
}

pub fn check_plan_code(code: &str) -> Result<(), ValidationError> {
    validate_plan_code(code)
}

fn validate_plan_name(value: &Value) -> Result<(), ValidationError> {
    validate_localized_map(value, &["en", "pt-BR"], 1, 60)
}

fn validate_plan_description(value: &Value) -> Result<(), ValidationError> {
    validate_localized_map(value, &[], 0, 400)
}

fn validate_feature_map(value: &BTreeMap<String, bool>) -> Result<(), ValidationError> {
    for key in value.keys() {
        if !Feature::ALL.iter().any(|f| f.key() == key) {
            return Err(error(
                "unknown_feature",
                format!("Unknown feature '{key}'."),
            ));
        }
    }
    Ok(())
}

fn validate_currency(value: &str) -> Result<(), ValidationError> {
    if value.len() == 3 && value.chars().all(|c| c.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(error(
            "invalid_currency",
            "Currency must be an ISO 4217 code.",
        ))
    }
}

/// Smallest amount Stripe charges in BRL (R$ 0,50).
pub const MIN_CHARGE_CENTS: i32 = 50;

/// A price is either free (`0`, not sold for that interval) or at least
/// the provider's minimum charge.
fn validate_chargeable_price(cents: i32) -> Result<(), ValidationError> {
    if cents == 0 || cents >= MIN_CHARGE_CENTS {
        Ok(())
    } else {
        Err(error(
            "below_minimum_charge",
            format!("A price must be 0 (not sold) or at least {MIN_CHARGE_CENTS} cents."),
        ))
    }
}

/// Body of `PUT /admin/plans/{code}`. Omitted fields keep their value; a
/// new plan needs at least `name`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct UpsertPlanPayload {
    #[validate(custom(function = "validate_plan_name"))]
    pub name: Option<Value>,
    #[validate(custom(function = "validate_plan_description"))]
    pub description: Option<Value>,
    #[validate(
        range(min = 0, max = 100_000_000),
        custom(function = "validate_chargeable_price")
    )]
    pub price_monthly_cents: Option<i32>,
    #[validate(
        range(min = 0, max = 1_000_000_000),
        custom(function = "validate_chargeable_price")
    )]
    pub price_yearly_cents: Option<i32>,
    #[validate(custom(function = "validate_currency"))]
    pub currency: Option<String>,
    #[validate(nested)]
    pub limits: Option<QuotaLimits>,
    #[validate(custom(function = "validate_feature_map"))]
    pub features: Option<BTreeMap<String, bool>>,
    pub highlighted: Option<bool>,
    pub is_public: Option<bool>,
    #[validate(range(min = -10_000, max = 10_000))]
    pub sort_order: Option<i32>,
}

// ---------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "subscription_status", rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Trialing,
    Active,
    PastDue,
    Canceled,
    Expired,
}

impl SubscriptionStatus {
    pub fn key(&self) -> &'static str {
        match self {
            SubscriptionStatus::Trialing => "trialing",
            SubscriptionStatus::Active => "active",
            SubscriptionStatus::PastDue => "past_due",
            SubscriptionStatus::Canceled => "canceled",
            SubscriptionStatus::Expired => "expired",
        }
    }

    /// Statuses that grant the plan (until the period ends).
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            SubscriptionStatus::Trialing | SubscriptionStatus::Active | SubscriptionStatus::PastDue
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "subscription_source", rename_all = "snake_case")]
pub enum SubscriptionSource {
    Trial,
    Admin,
    PromoCode,
    Credits,
    Referral,
    Payment,
}

/// Days a paid subscription keeps its plan past `current_period_end`, so a
/// renewal confirmed late by the payment provider never interrupts access.
pub const RENEWAL_GRACE_DAYS: i64 = 2;

/// Days a paid subscription whose renewal failed (`past_due`) keeps its
/// plan while the provider retries the charge, counted from the failure
/// (`past_due_since`). Matches the Subscription Terms ("novas tentativas
/// por até 14 dias") and the Stripe retry schedule; afterwards the account
/// has no plan and the subscription is canceled at the provider.
pub const PAYMENT_GRACE_DAYS: i64 = 14;

/// Days after a paid invoice during which the buyer may withdraw with a
/// full refund (CDC art. 49).
pub const WITHDRAWAL_DAYS: i64 = 7;

/// `true` while a subscription with these fields grants its plan at `now`.
pub fn subscription_in_effect(
    status: SubscriptionStatus,
    source: SubscriptionSource,
    current_period_end: Option<NaiveDateTime>,
    past_due_since: Option<NaiveDateTime>,
    now: NaiveDateTime,
) -> bool {
    let grace = match source {
        SubscriptionSource::Payment => chrono::Duration::days(RENEWAL_GRACE_DAYS),
        _ => chrono::Duration::zero(),
    };
    let retrying = status != SubscriptionStatus::PastDue
        || past_due_since
            .is_none_or(|since| since + chrono::Duration::days(PAYMENT_GRACE_DAYS) > now);
    status.is_live() && retrying && current_period_end.is_none_or(|end| end + grace > now)
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct Subscription {
    pub plan_code: String,
    pub status: SubscriptionStatus,
    pub source: SubscriptionSource,
    pub started_at: NaiveDateTime,
    /// `None` = open-ended. For a paid subscription: the next charge (or,
    /// with `cancel_at_period_end`, when it ends).
    pub current_period_end: Option<NaiveDateTime>,
    pub trial_ends_at: Option<NaiveDateTime>,
    pub cancel_at_period_end: bool,
    /// `monthly` or `yearly` for paid subscriptions.
    #[sqlx(default)]
    pub billing_interval: Option<String>,
    /// When a paid subscription's renewal failed (`status = past_due`).
    #[sqlx(default)]
    pub past_due_since: Option<NaiveDateTime>,
}

impl Subscription {
    /// `true` while the subscription grants its plan at `now`.
    pub fn is_effective(&self, now: NaiveDateTime) -> bool {
        subscription_in_effect(
            self.status,
            self.source,
            self.current_period_end,
            self.past_due_since,
            now,
        )
    }
}

/// How often a paid plan is charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BillingInterval {
    Monthly,
    Yearly,
}

impl BillingInterval {
    pub fn key(&self) -> &'static str {
        match self {
            BillingInterval::Monthly => "monthly",
            BillingInterval::Yearly => "yearly",
        }
    }

    /// The interval as the payment provider names it.
    pub fn provider_key(&self) -> &'static str {
        match self {
            BillingInterval::Monthly => "month",
            BillingInterval::Yearly => "year",
        }
    }

    pub fn from_provider(value: &str) -> Option<Self> {
        match value {
            "month" => Some(BillingInterval::Monthly),
            "year" => Some(BillingInterval::Yearly),
            _ => None,
        }
    }

    /// The plan's price for this interval, in cents.
    pub fn price_of(&self, plan: &Plan) -> i32 {
        match self {
            BillingInterval::Monthly => plan.price_monthly_cents,
            BillingInterval::Yearly => plan.price_yearly_cents,
        }
    }
}

/// Body of `POST /billing/checkout` and `POST /billing/subscription/change`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct CheckoutPayload {
    #[validate(length(min = 2, max = 32))]
    pub plan_code: String,
    pub interval: BillingInterval,
}

/// Where to send the browser next (a page hosted by the payment provider).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RedirectResponse {
    pub url: String,
}

/// One change in a subscription's history.
#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct SubscriptionEvent {
    pub id: Uuid,
    pub kind: String,
    pub from_plan: Option<String>,
    pub to_plan: Option<String>,
    pub from_status: Option<SubscriptionStatus>,
    pub to_status: Option<SubscriptionStatus>,
    pub data: Value,
    pub actor_username: Option<String>,
    pub created_at: NaiveDateTime,
}

/// The keys of [`SubscriptionEvent::data`] an account may see about its
/// own history. Everything else stays with staff: the note an admin
/// wrote when granting a plan, the reason of a staff refund, provider
/// object ids and the like.
const OWNER_VISIBLE_EVENT_DATA: &[&str] = &[
    "days",
    "source",
    "plan_code",
    "interval",
    "effective_at",
    "current_period_end",
    "currency",
    "refunded_cents",
    "old_amount_cents",
    "new_amount_cents",
];

/// One change in the caller's own subscription history
/// (`GET /billing/history`): a [`SubscriptionEvent`] without who did it
/// and without the staff-only details in `data`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OwnSubscriptionEvent {
    pub id: Uuid,
    pub kind: String,
    pub from_plan: Option<String>,
    pub to_plan: Option<String>,
    pub from_status: Option<SubscriptionStatus>,
    pub to_status: Option<SubscriptionStatus>,
    pub data: Value,
    pub created_at: NaiveDateTime,
}

impl From<SubscriptionEvent> for OwnSubscriptionEvent {
    fn from(event: SubscriptionEvent) -> Self {
        let data = match event.data {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .filter(|(key, _)| OWNER_VISIBLE_EVENT_DATA.contains(&key.as_str()))
                    .collect(),
            ),
            _ => Value::Object(Default::default()),
        };
        Self {
            id: event.id,
            kind: event.kind,
            from_plan: event.from_plan,
            to_plan: event.to_plan,
            from_status: event.from_status,
            to_status: event.to_status,
            data,
            created_at: event.created_at,
        }
    }
}

// ---------------------------------------------------------------------
// The caller's billing (`GET /billing/me`)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreditsSummary {
    pub balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReferralSummary {
    pub code: Option<String>,
    /// `/register?ref=CODE`.
    pub link_path: Option<String>,
    pub rewarded_count: i64,
    pub pending_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BillingMe {
    pub enforced: bool,
    /// Which rules apply to the caller (`staff`, `plan`, `unverified`,
    /// `beta`, `free`).
    pub access: AccessTier,
    pub email_verified: bool,
    /// Whether the caller may buy or be granted a plan (staff can't: they
    /// already have everything).
    pub can_subscribe: bool,
    /// Card payments are configured (checkout and the billing portal work).
    pub payments_enabled: bool,
    /// The plan in effect, if any.
    pub plan: Option<Plan>,
    pub subscription: Option<Subscription>,
    /// What the caller may use right now.
    pub features: BTreeMap<String, bool>,
    pub credits: CreditsSummary,
    pub referral: ReferralSummary,
    pub rewards: Vec<Reward>,
    /// Until when the current paid subscription may be withdrawn from with
    /// a full refund (`POST /billing/withdraw`): 7 days from its first paid
    /// invoice (or from a yearly renewal charge). `null` outside that
    /// window.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub withdrawal_eligible_until: Option<DateTime<Utc>>,
    /// When the last renewal charge failed, while the paid subscription is
    /// `past_due` (the plan is kept for 14 days from here).
    #[schema(value_type = Option<String>, format = DateTime)]
    pub past_due_since: Option<DateTime<Utc>>,
}

/// Body of `POST /admin/users/{id}/subscription/refund`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct StaffRefundPayload {
    /// Why the subscription is refunded (1 to 500 characters). Kept in the
    /// audit log and the subscription's history.
    #[validate(
        length(min = 1),
        custom(function = "crate::validations::text::validate_reason")
    )]
    pub reason: String,
}

/// Answer of `POST /billing/withdraw`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WithdrawResponse {
    /// Minor units refunded to the card.
    pub refunded_cents: i64,
    /// ISO 4217, lower case (`brl`).
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct RedeemCodePayload {
    #[validate(length(min = 1, max = 32))]
    pub code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "promo_kind", rename_all = "snake_case")]
pub enum PromoKind {
    PlanGrant,
    TrialExtension,
    Credits,
    Discount,
}

/// What a redeemed promo code did.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RedemptionSummary {
    pub kind: PromoKind,
    pub plan_code: Option<String>,
    pub days: Option<i32>,
    pub credits: Option<i32>,
    /// For `discount` codes: applied to the next payment, not now.
    pub discount_percent: Option<i32>,
}

/// Answer of `POST /billing/redeem`: the updated billing state plus what
/// the code did.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RedeemResponse {
    #[serde(flatten)]
    pub billing: BillingMe,
    pub redemption: RedemptionSummary,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct CreditEntry {
    pub id: Uuid,
    pub amount: i32,
    pub reason: String,
    pub note: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct RedeemRewardPayload {
    #[validate(length(min = 1, max = 40))]
    pub reward_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, ToSchema)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "referral_status", rename_all = "snake_case")]
pub enum ReferralStatus {
    Pending,
    Rewarded,
    Rejected,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct ReferralEntry {
    pub username: String,
    pub status: ReferralStatus,
    pub created_at: NaiveDateTime,
    pub rewarded_at: Option<NaiveDateTime>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BillingPageQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

// ---------------------------------------------------------------------
// Staff
// ---------------------------------------------------------------------

/// `GET /admin/users/{id}/subscription`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminSubscriptionView {
    pub subscription: Option<Subscription>,
    pub events: Vec<SubscriptionEvent>,
    pub credits_balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct GrantSubscriptionPayload {
    #[validate(length(min = 1, max = 32))]
    pub plan_code: String,
    /// `null` = open-ended.
    #[validate(range(min = 1, max = 3650))]
    pub days: Option<i64>,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

fn validate_credit_amount(amount: i32) -> Result<(), ValidationError> {
    if amount != 0 && (-100_000..=100_000).contains(&amount) {
        Ok(())
    } else {
        Err(error(
            "invalid_amount",
            "Amount must be between -100000 and 100000 and not zero.",
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct AdjustCreditsPayload {
    #[validate(custom(function = "validate_credit_amount"))]
    pub amount: i32,
    #[validate(length(min = 1, max = 255))]
    pub note: String,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct PromoCode {
    pub id: Uuid,
    pub code: String,
    pub description: Option<String>,
    pub kind: PromoKind,
    pub plan_code: Option<String>,
    pub duration_days: Option<i32>,
    pub credits: Option<i32>,
    pub discount_percent: Option<i32>,
    pub max_redemptions: Option<i32>,
    pub redemptions_count: i32,
    pub new_users_only: bool,
    pub starts_at: Option<NaiveDateTime>,
    pub expires_at: Option<NaiveDateTime>,
    pub disabled_at: Option<NaiveDateTime>,
    pub created_by_username: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

fn validate_promo_code(code: &str) -> Result<(), ValidationError> {
    let code = code.trim();
    if (MIN_PROMO_CODE_CHARS..=32).contains(&code.len())
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(error(
            "invalid_promo_code",
            "Codes have 8 to 32 letters, digits, '-' or '_'.",
        ))
    }
}

/// Shortest custom code accepted, whatever it grants: credits and trial
/// time are worth money too, and redemption attempts are throttled, not
/// impossible (generated codes have 10 characters).
pub const MIN_PROMO_CODE_CHARS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct CreatePromoCodePayload {
    /// Generated when omitted. Stored upper-case.
    #[validate(custom(function = "validate_promo_code"))]
    pub code: Option<String>,
    #[validate(length(max = 255))]
    pub description: Option<String>,
    pub kind: PromoKind,
    /// `plan_grant` only.
    #[validate(length(min = 1, max = 32))]
    pub plan_code: Option<String>,
    /// `plan_grant` and `trial_extension`.
    #[validate(range(min = 1, max = 3650))]
    pub duration_days: Option<i32>,
    /// `credits` only.
    #[validate(range(min = 1, max = 100_000))]
    pub credits: Option<i32>,
    /// `discount` only.
    #[validate(range(min = 1, max = 100))]
    pub discount_percent: Option<i32>,
    #[validate(range(min = 1, max = 1_000_000))]
    pub max_redemptions: Option<i32>,
    #[serde(default)]
    pub new_users_only: bool,
    pub starts_at: Option<NaiveDateTime>,
    pub expires_at: Option<NaiveDateTime>,
}

impl CreatePromoCodePayload {
    /// Checks that exactly the fields of `kind` are present.
    pub fn check_kind_fields(&self) -> Result<(), String> {
        let (plan, days, credits, discount) = match self.kind {
            PromoKind::PlanGrant => (true, true, false, false),
            PromoKind::TrialExtension => (false, true, false, false),
            PromoKind::Credits => (false, false, true, false),
            PromoKind::Discount => (false, false, false, true),
        };
        let checks = [
            ("plan_code", plan, self.plan_code.is_some()),
            ("duration_days", days, self.duration_days.is_some()),
            ("credits", credits, self.credits.is_some()),
            (
                "discount_percent",
                discount,
                self.discount_percent.is_some(),
            ),
        ];
        for (field, required, present) in checks {
            if required && !present {
                return Err(format!("'{field}' is required for this kind of code."));
            }
            if !required && present {
                return Err(format!("'{field}' does not apply to this kind of code."));
            }
        }
        if let (Some(start), Some(end)) = (self.starts_at, self.expires_at)
            && end <= start
        {
            return Err("'expires_at' must be after 'starts_at'.".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, Validate)]
pub struct UpdatePromoCodePayload {
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<NaiveDateTime>)]
    pub expires_at: Option<Option<NaiveDateTime>>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<i32>)]
    pub max_redemptions: Option<Option<i32>>,
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct PromoRedemption {
    pub user_id: Uuid,
    pub username: String,
    pub redeemed_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, ToSchema)]
pub struct Promotion {
    pub id: Uuid,
    pub name: String,
    pub headline: Value,
    /// `None` = every paid plan.
    pub plan_code: Option<String>,
    pub discount_percent: i32,
    pub starts_at: NaiveDateTime,
    pub ends_at: NaiveDateTime,
    pub active: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

fn validate_headline(value: &Value) -> Result<(), ValidationError> {
    validate_localized_map(value, &[], 1, 120)
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct CreatePromotionPayload {
    #[validate(length(min = 1, max = 80))]
    pub name: String,
    #[validate(custom(function = "validate_headline"))]
    pub headline: Value,
    #[validate(length(min = 1, max = 32))]
    pub plan_code: Option<String>,
    #[validate(range(min = 1, max = 100))]
    pub discount_percent: i32,
    pub starts_at: NaiveDateTime,
    pub ends_at: NaiveDateTime,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, Validate)]
pub struct UpdatePromotionPayload {
    #[validate(length(min = 1, max = 80))]
    pub name: Option<String>,
    #[validate(custom(function = "validate_headline"))]
    pub headline: Option<Value>,
    #[serde(default, deserialize_with = "crate::models::patch::double_option")]
    #[schema(value_type = Option<String>)]
    pub plan_code: Option<Option<String>>,
    #[validate(range(min = 1, max = 100))]
    pub discount_percent: Option<i32>,
    pub starts_at: Option<NaiveDateTime>,
    pub ends_at: Option<NaiveDateTime>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Validate)]
pub struct GrantTrialsPayload {
    #[validate(range(min = 1, max = 365))]
    pub days: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GrantTrialsResponse {
    pub granted: i64,
}

/// `GET /admin/billing/overview`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BillingOverview {
    pub enforced: bool,
    /// Subscriptions per status.
    pub by_status: BTreeMap<String, i64>,
    /// Subscriptions currently granting each plan.
    pub by_plan: BTreeMap<String, i64>,
    pub accounts_without_subscription: i64,
    /// Credits ever granted (positive ledger entries).
    pub credits_issued: i64,
    /// Credits ever spent or removed (negative entries, as a positive number).
    pub credits_spent: i64,
    pub promo_redemptions_last_30_days: i64,
    pub referrals_rewarded: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PromoListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    /// Case-insensitive search over code and description.
    pub q: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_default_and_partial_json() {
        let settings: BillingSettings =
            serde_json::from_value(json!({ "enforced": true })).unwrap();
        assert!(settings.enforced);
        assert_eq!(settings.trial_days, 30);
        assert_eq!(settings.rewards.len(), 2);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn localized_maps_are_validated() {
        assert!(
            validate_localized_map(
                &json!({"en": "Pro", "pt-BR": "Pro"}),
                &["en", "pt-BR"],
                1,
                10
            )
            .is_ok()
        );
        assert!(validate_localized_map(&json!({"en": "Pro"}), &["en", "pt-BR"], 1, 10).is_err());
        assert!(validate_localized_map(&json!({"fr": "Pro"}), &[], 1, 10).is_err());
        assert!(validate_localized_map(&json!({"en": 1}), &[], 1, 10).is_err());
        assert!(validate_localized_map(&json!("x"), &[], 1, 10).is_err());
    }

    #[test]
    fn promo_kinds_require_their_fields() {
        let mut payload = CreatePromoCodePayload {
            code: None,
            description: None,
            kind: PromoKind::Credits,
            plan_code: None,
            duration_days: None,
            credits: Some(10),
            discount_percent: None,
            max_redemptions: None,
            new_users_only: false,
            starts_at: None,
            expires_at: None,
        };
        assert!(payload.check_kind_fields().is_ok());
        payload.duration_days = Some(3);
        assert!(payload.check_kind_fields().is_err());
        payload.kind = PromoKind::PlanGrant;
        payload.credits = None;
        assert!(payload.check_kind_fields().is_err());
        payload.plan_code = Some("pro".into());
        assert!(payload.check_kind_fields().is_ok());
    }

    #[test]
    fn plan_codes_and_features() {
        assert!(check_plan_code("pro").is_ok());
        assert!(check_plan_code("none").is_err());
        assert!(check_plan_code("Pro").is_err());
        let features = normalize_features(&json!({ "tours": true, "bogus": true }));
        assert_eq!(features.len(), Feature::ALL.len());
        assert!(features["tours"]);
        assert!(!features["create_bands"]);
        let mut map = BTreeMap::new();
        map.insert("bogus".to_string(), true);
        assert!(validate_feature_map(&map).is_err());
    }
}
