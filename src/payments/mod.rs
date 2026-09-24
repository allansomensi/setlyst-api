//! Card payments.
//!
//! [`PaymentGateway`] is everything the API needs from the payment
//! provider; [`stripe::StripeGateway`] implements it over Stripe's REST API
//! and tests substitute a fake. The subscription logic built on top lives
//! in `services::payments`.
//!
//! Plans and prices stay in the database (the staff console edits them);
//! the gateway mirrors them into the provider on demand, so there is
//! nothing to set up by hand in the provider's dashboard.

pub mod stripe;
pub mod webhook;

use crate::{
    config::Config,
    errors::api_error::{ApiError, codes},
    models::billing::BillingInterval,
};
use axum::http::StatusCode;
use std::sync::Arc;
use tracing::error;
use uuid::Uuid;

/// A failed call to the payment provider.
#[derive(Debug, Clone)]
pub struct PaymentError {
    /// HTTP status of the provider's answer (`None` = no answer).
    pub status: Option<u16>,
    /// The provider's machine-readable error code, if any.
    pub code: Option<String>,
    pub message: String,
}

impl PaymentError {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            status: None,
            code: None,
            message: message.into(),
        }
    }

    /// The object named in the request doesn't exist at the provider
    /// (deleted in the dashboard, or never existed in this mode).
    pub fn is_missing(&self) -> bool {
        self.code.as_deref() == Some("resource_missing")
    }

    /// The key is valid but not allowed to make this call (a restricted
    /// key without the needed scope): HTTP 403.
    pub fn is_permission_denied(&self) -> bool {
        self.status == Some(403)
    }
}

impl std::fmt::Display for PaymentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.status, &self.code) {
            (Some(status), Some(code)) => write!(f, "{status} {code}: {}", self.message),
            (Some(status), None) => write!(f, "{status}: {}", self.message),
            _ => write!(f, "{}", self.message),
        }
    }
}

impl From<PaymentError> for ApiError {
    /// Provider details are logged, never shown: the client gets a stable
    /// code and a generic message.
    fn from(e: PaymentError) -> Self {
        error!(error = %e, "Payment provider request failed");
        ApiError::rule(
            StatusCode::BAD_GATEWAY,
            codes::PAYMENT_PROVIDER_ERROR,
            "The payment service could not complete the request. Try again in a moment.",
        )
    }
}

/// A plan price as the provider bills it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceSpec {
    pub plan_code: String,
    /// Shown on the provider's checkout page and invoices.
    pub product_name: String,
    pub interval: BillingInterval,
    /// ISO 4217, upper case (as stored on plans).
    pub currency: String,
    pub unit_amount: i64,
}

impl PriceSpec {
    /// Stable key of this exact price: a changed amount is a new price, so
    /// current subscribers keep what they signed up for.
    pub fn lookup_key(&self) -> String {
        format!(
            "setlyst_{}_{}_{}_{}",
            self.plan_code,
            self.interval.provider_key(),
            self.unit_amount,
            self.currency.to_ascii_lowercase()
        )
    }
}

/// Everything needed to open a hosted checkout page.
#[derive(Debug, Clone)]
pub struct CheckoutRequest {
    pub user_id: Uuid,
    pub customer_id: String,
    pub price_id: String,
    pub plan_code: String,
    pub interval: BillingInterval,
    pub success_url: String,
    pub cancel_url: String,
    /// Checkout page language (`en`, `pt-BR`, `es`).
    pub locale: String,
    /// Unix time the first charge is deferred to (the rest of an in-app
    /// trial).
    pub trial_end: Option<i64>,
    /// One-off discount on the first charge.
    pub coupon_id: Option<String>,
    /// The redeemed `discount` promo code the coupon comes from.
    pub redemption_id: Option<Uuid>,
    /// Text shown next to the required Terms of Service checkbox (with a
    /// link to the Subscription Terms), in the checkout language.
    pub terms_message: Option<String>,
    /// Version of the terms the buyer accepts, stored from the completed
    /// checkout.
    pub terms_version: Option<String>,
    /// Stable per attempt, so a retried request never opens a second page.
    pub idempotency_key: String,
}

/// A hosted checkout page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutSession {
    pub id: String,
    pub url: String,
}

/// A subscription as the provider reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GatewaySubscription {
    pub id: String,
    pub customer_id: String,
    /// The provider's status (`active`, `trialing`, `past_due`,
    /// `canceled`, `unpaid`, `paused`, `incomplete`, `incomplete_expired`).
    pub status: String,
    pub item_id: String,
    pub price_id: String,
    pub plan_code: Option<String>,
    pub interval: Option<BillingInterval>,
    /// Unix time.
    pub current_period_end: Option<i64>,
    pub cancel_at_period_end: bool,
    /// Unix time.
    pub canceled_at: Option<i64>,
    /// Unix time.
    pub ended_at: Option<i64>,
    /// The account the subscription was bought for (checkout metadata).
    pub user_id: Option<Uuid>,
    /// A plan change is waiting for its payment (it failed or needs the
    /// customer's action); the subscription still has its old price.
    pub has_pending_update: bool,
    /// Unix time the provider-side trial ends (the first charge).
    pub trial_end: Option<i64>,
    /// The item's price, minor units, as charged (before discounts).
    pub unit_amount: Option<i64>,
    /// ISO 4217, upper case.
    pub currency: Option<String>,
    /// The subscription's most recent invoice.
    pub latest_invoice_id: Option<String>,
    /// That invoice's status (`open`, `paid`...), when it was expanded.
    pub latest_invoice_status: Option<String>,
    /// Page where the customer can pay (or authenticate) that invoice.
    pub hosted_invoice_url: Option<String>,
    /// Status of the payment of that invoice (`requires_action`,
    /// `requires_payment_method`...), when it was expanded.
    pub latest_payment_status: Option<String>,
}

impl GatewaySubscription {
    /// Statuses in which the provider may still charge (or is about to):
    /// a second subscription in one of them is a double charge.
    pub fn may_charge(&self) -> bool {
        matches!(
            self.status.as_str(),
            "active" | "trialing" | "past_due" | "unpaid" | "incomplete"
        )
    }
}

/// One page of subscriptions, newest first.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionPage {
    pub subscriptions: Vec<GatewaySubscription>,
    pub last_id: Option<String>,
    pub has_more: bool,
}

/// A paid invoice as the provider reports it: one payment received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayInvoice {
    pub id: String,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    /// The account the subscription was bought for (subscription metadata).
    pub user_id: Option<Uuid>,
    /// The payment that settled it, used to match refunds.
    pub payment_intent_id: Option<String>,
    pub plan_code: Option<String>,
    pub interval: Option<BillingInterval>,
    /// Minor units actually paid (after discounts).
    pub amount_paid: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    /// Unix time.
    pub paid_at: Option<i64>,
}

/// One page of paid invoices, newest first.
#[derive(Debug, Clone, Default)]
pub struct InvoicePage {
    pub invoices: Vec<GatewayInvoice>,
    /// Id of the last invoice on the page (paid or not): where the next
    /// page starts.
    pub last_id: Option<String>,
    pub has_more: bool,
}

/// A refund as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayRefund {
    pub id: String,
    pub payment_intent_id: Option<String>,
    pub charge_id: Option<String>,
    /// Minor units.
    pub amount: i64,
    /// ISO 4217, upper case.
    pub currency: Option<String>,
    /// `succeeded`, `pending`, `failed`, `canceled`…
    pub status: String,
    /// Unix time.
    pub created: Option<i64>,
}

impl GatewayRefund {
    /// Refunds that give (or are giving) money back.
    pub fn counts(&self) -> bool {
        matches!(self.status.as_str(), "succeeded" | "pending")
    }
}

/// Stripe's fee on a payment, from its balance transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentFees {
    pub charge_id: Option<String>,
    /// Minor units.
    pub fee: i64,
    /// What is paid out: amount minus fee.
    pub net: i64,
}

/// One page of refunds, newest first.
#[derive(Debug, Clone, Default)]
pub struct RefundPage {
    pub refunds: Vec<GatewayRefund>,
    pub last_id: Option<String>,
    pub has_more: bool,
}

/// What the API needs from a payment provider.
#[async_trait::async_trait]
pub trait PaymentGateway: Send + Sync {
    /// Creates the provider-side customer of an account. `generation`
    /// goes into the idempotency key: it changes when the previous
    /// customer was deleted at the provider.
    async fn create_customer(
        &self,
        user_id: Uuid,
        email: Option<&str>,
        name: &str,
        locale: &str,
        generation: i32,
    ) -> Result<String, PaymentError>;

    /// Updates the contact details of a customer (receipts, dunning and
    /// 3DS links go to this address).
    async fn update_customer(
        &self,
        _customer_id: &str,
        _email: Option<&str>,
        _name: &str,
    ) -> Result<(), PaymentError> {
        Ok(())
    }

    /// The id of the provider price for `spec`, creating it (and its
    /// product) when missing.
    async fn ensure_price(&self, spec: &PriceSpec) -> Result<String, PaymentError>;

    /// A single-use coupon taking `percent` off the first charge.
    async fn create_coupon(
        &self,
        percent: i32,
        name: &str,
        idempotency_key: &str,
    ) -> Result<String, PaymentError>;

    /// Opens a hosted checkout page.
    async fn create_checkout(
        &self,
        request: &CheckoutRequest,
    ) -> Result<CheckoutSession, PaymentError>;

    /// Expires a checkout page that is still open, so it can no longer be
    /// paid. One already completed or expired is not an error.
    async fn expire_checkout(&self, _session_id: &str) -> Result<(), PaymentError> {
        Ok(())
    }

    /// Subscriptions of every status, newest first: of one customer, or
    /// of the whole account (`None`), after `starting_after`.
    async fn list_subscriptions(
        &self,
        _customer_id: Option<&str>,
        _starting_after: Option<&str>,
    ) -> Result<SubscriptionPage, PaymentError> {
        Ok(SubscriptionPage::default())
    }

    /// Opens the hosted billing portal (payment method, invoices,
    /// cancellation); returns its URL.
    async fn create_portal(
        &self,
        customer_id: &str,
        return_url: &str,
        locale: &str,
    ) -> Result<String, PaymentError>;

    async fn get_subscription(&self, id: &str) -> Result<GatewaySubscription, PaymentError>;

    /// Moves the subscription to `price_id`, charging the prorated
    /// difference now. The change only applies if that charge succeeds
    /// (otherwise `has_pending_update` is set on the result).
    async fn change_price(
        &self,
        subscription_id: &str,
        item_id: &str,
        price_id: &str,
    ) -> Result<GatewaySubscription, PaymentError>;

    /// Ends the subscription immediately (no further charges). A
    /// subscription that has already ended is not an error.
    async fn cancel_now(&self, subscription_id: &str) -> Result<(), PaymentError>;

    /// A paid invoice, or `None` when it isn't (or no longer) paid.
    async fn get_paid_invoice(&self, _id: &str) -> Result<Option<GatewayInvoice>, PaymentError> {
        Ok(None)
    }

    /// The paid invoices (amount above zero) of one subscription, newest
    /// first.
    async fn list_subscription_invoices(
        &self,
        _subscription_id: &str,
    ) -> Result<Vec<GatewayInvoice>, PaymentError> {
        Ok(Vec::new())
    }

    /// Refunds a payment (intent): `amount` minor units, or everything
    /// still refundable. The same `idempotency_key` never refunds twice.
    async fn refund(
        &self,
        payment_intent_id: &str,
        amount: Option<i64>,
        idempotency_key: &str,
    ) -> Result<GatewayRefund, PaymentError>;

    /// The refunds of one payment (intent).
    async fn list_refunds_for_payment(
        &self,
        _payment_intent_id: &str,
    ) -> Result<Vec<GatewayRefund>, PaymentError> {
        Ok(Vec::new())
    }

    /// Stripe's fee on a payment, when its balance transaction exists.
    async fn payment_fees(
        &self,
        _payment_intent_id: &str,
    ) -> Result<Option<PaymentFees>, PaymentError> {
        Ok(None)
    }

    /// Checks that the API key can read what the webhook and the finance
    /// report need. Returns the scopes that failed, with the error.
    async fn probe_permissions(&self) -> Vec<(String, PaymentError)> {
        Vec::new()
    }

    /// Paid invoices, newest first, after the invoice `starting_after`.
    async fn list_paid_invoices(
        &self,
        _starting_after: Option<&str>,
    ) -> Result<InvoicePage, PaymentError> {
        Ok(InvoicePage::default())
    }

    /// The invoice a payment (intent) settled, if it settled one.
    async fn invoice_for_payment(
        &self,
        _payment_intent_id: &str,
    ) -> Result<Option<String>, PaymentError> {
        Ok(None)
    }

    /// Refunds, newest first, after the refund `starting_after`.
    async fn list_refunds(
        &self,
        _starting_after: Option<&str>,
    ) -> Result<RefundPage, PaymentError> {
        Ok(RefundPage::default())
    }

    /// Deletes the provider-side customer (its payment methods and any
    /// subscription left). Invoices stay with the provider, as the law
    /// requires. A customer that no longer exists is not an error.
    async fn delete_customer(&self, _customer_id: &str) -> Result<(), PaymentError> {
        Ok(())
    }
}

/// The configured payment integration.
#[derive(Clone)]
pub struct Payments {
    pub gateway: Arc<dyn PaymentGateway>,
    /// Signing secret of the webhook endpoint.
    pub webhook_secret: String,
    /// The API key is a live-mode key: webhook events must be live too
    /// (a test event reaching the live endpoint, or the reverse, is
    /// ignored).
    pub livemode: bool,
}

impl Payments {
    /// The Stripe integration, when the configuration has one.
    pub fn from_config(config: &Config) -> Option<Self> {
        let stripe = config.stripe.as_ref()?;
        match stripe::StripeGateway::new(stripe) {
            Ok(gateway) => Some(Self {
                gateway: Arc::new(gateway),
                webhook_secret: stripe.webhook_secret.clone(),
                livemode: !stripe.is_test_mode(),
            }),
            Err(e) => {
                error!(error = %e, "Could not build the Stripe client; payments are disabled");
                None
            }
        }
    }
}

/// The payment provider's page language for an app locale.
pub fn provider_locale(locale: &str) -> &'static str {
    match locale {
        "pt-BR" => "pt-BR",
        "es" => "es",
        _ => "en",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_keys_change_with_the_amount() {
        let spec = PriceSpec {
            plan_code: "pro".into(),
            product_name: "Setlyst Pro".into(),
            interval: BillingInterval::Yearly,
            currency: "BRL".into(),
            unit_amount: 39900,
        };
        assert_eq!(spec.lookup_key(), "setlyst_pro_year_39900_brl");
        let cheaper = PriceSpec {
            unit_amount: 29900,
            ..spec.clone()
        };
        assert_ne!(spec.lookup_key(), cheaper.lookup_key());
    }

    #[test]
    fn provider_errors_never_leak_to_clients() {
        let error: ApiError = PaymentError {
            status: Some(401),
            code: Some("api_key_expired".into()),
            message: "Expired API Key provided: sk_live_***".into(),
        }
        .into();
        let text = error.to_string();
        assert!(!text.contains("sk_live"), "{text}");
        assert!(matches!(
            error,
            ApiError::Rule {
                code: codes::PAYMENT_PROVIDER_ERROR,
                ..
            }
        ));
    }

    #[test]
    fn locales_map_to_provider_languages() {
        assert_eq!(provider_locale("pt-BR"), "pt-BR");
        assert_eq!(provider_locale("es"), "es");
        assert_eq!(provider_locale("en"), "en");
        assert_eq!(provider_locale("fr"), "en");
    }
}
