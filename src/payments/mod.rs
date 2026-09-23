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
}

/// A subscription as the provider reports it.
#[derive(Debug, Clone, PartialEq)]
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
}

/// What the API needs from a payment provider.
#[async_trait::async_trait]
pub trait PaymentGateway: Send + Sync {
    /// Creates the provider-side customer of an account.
    async fn create_customer(
        &self,
        user_id: Uuid,
        email: Option<&str>,
        name: &str,
        locale: &str,
    ) -> Result<String, PaymentError>;

    /// The id of the provider price for `spec`, creating it (and its
    /// product) when missing.
    async fn ensure_price(&self, spec: &PriceSpec) -> Result<String, PaymentError>;

    /// A single-use coupon taking `percent` off the first charge.
    async fn create_coupon(&self, percent: i32, name: &str) -> Result<String, PaymentError>;

    /// Opens a hosted checkout page; returns its URL.
    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<String, PaymentError>;

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
}

/// The configured payment integration.
#[derive(Clone)]
pub struct Payments {
    pub gateway: Arc<dyn PaymentGateway>,
    /// Signing secret of the webhook endpoint.
    pub webhook_secret: String,
}

impl Payments {
    /// The Stripe integration, when the configuration has one.
    pub fn from_config(config: &Config) -> Option<Self> {
        let stripe = config.stripe.as_ref()?;
        match stripe::StripeGateway::new(stripe) {
            Ok(gateway) => Some(Self {
                gateway: Arc::new(gateway),
                webhook_secret: stripe.webhook_secret.clone(),
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
