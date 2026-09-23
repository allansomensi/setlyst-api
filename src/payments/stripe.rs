//! [`PaymentGateway`] over Stripe's REST API.
//!
//! Requests are form-encoded, as Stripe expects, and pinned to
//! [`API_VERSION`] so response shapes never change under us when the
//! account's default version is upgraded. Only the fields the API uses are
//! read from responses.

use super::{
    CheckoutRequest, GatewaySubscription, PaymentError, PaymentGateway, PriceSpec, provider_locale,
};
use crate::{config::StripeConfig, models::billing::BillingInterval};
use chrono::Utc;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tracing::debug;
use uuid::Uuid;

/// The Stripe API version every request is made with. Since this version
/// billing periods live on subscription items.
pub const API_VERSION: &str = "2025-03-31.basil";

/// Products are created with this id prefix plus the plan code.
pub const PRODUCT_PREFIX: &str = "setlyst_plan_";

type Params = Vec<(String, String)>;

fn param(key: &str, value: impl ToString) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// Stripe object ids are `[A-Za-z0-9_]`; anything else never reaches a URL.
fn checked_id(id: &str) -> Result<&str, PaymentError> {
    if !id.is_empty()
        && id.len() <= 255
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Ok(id)
    } else {
        Err(PaymentError::transport(format!("invalid Stripe id '{id}'")))
    }
}

pub struct StripeGateway {
    client: reqwest::Client,
    api_base: String,
    secret_key: String,
}

impl StripeGateway {
    pub fn new(config: &StripeConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("setlyst-api/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            api_base: config.api_base.clone(),
            secret_key: config.secret_key.clone(),
        })
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<Value, PaymentError> {
        let url = format!("{}{path}", self.api_base);
        let mut request = self
            .client
            .request(method.clone(), &url)
            .bearer_auth(&self.secret_key)
            .header("Stripe-Version", API_VERSION);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        request = if method == Method::GET {
            request.query(params)
        } else {
            request.form(params)
        };
        debug!(%method, path, "Stripe request");

        let response = request
            .send()
            .await
            .map_err(|e| PaymentError::transport(format!("Stripe unreachable: {e}")))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| PaymentError::transport(format!("unreadable Stripe response: {e}")))?;
        if status.is_success() {
            return Ok(body);
        }
        Err(PaymentError {
            status: Some(status.as_u16()),
            code: body
                .pointer("/error/code")
                .or_else(|| body.pointer("/error/type"))
                .and_then(Value::as_str)
                .map(str::to_string),
            message: body
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        })
    }

    async fn ensure_product(&self, spec: &PriceSpec) -> Result<String, PaymentError> {
        let id = format!("{PRODUCT_PREFIX}{}", spec.plan_code);
        let created = self
            .call(
                Method::POST,
                "/v1/products",
                &[
                    param("id", &id),
                    param("name", &spec.product_name),
                    param("metadata[plan_code]", &spec.plan_code),
                ],
                None,
            )
            .await;
        match created {
            Ok(_) => Ok(id),
            // Already created by an earlier checkout: the usual case.
            Err(e) if e.status == Some(StatusCode::BAD_REQUEST.as_u16()) => {
                let path = format!("/v1/products/{}", checked_id(&id)?);
                match self.call(Method::GET, &path, &[], None).await {
                    Ok(_) => Ok(id),
                    Err(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}

fn unix(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

/// Reads a subscription object.
pub fn parse_subscription(value: &Value) -> Result<GatewaySubscription, PaymentError> {
    let str_at = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
    let item = value
        .pointer("/items/data/0")
        .ok_or_else(|| PaymentError::transport("subscription without items"))?;
    let price = item
        .get("price")
        .ok_or_else(|| PaymentError::transport("subscription item without a price"))?;

    let product_id = match price.get("product") {
        Some(Value::String(id)) => Some(id.as_str()),
        Some(object) => object.get("id").and_then(Value::as_str),
        None => None,
    };
    let plan_code = price
        .pointer("/metadata/plan_code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| product_id.and_then(|id| id.strip_prefix(PRODUCT_PREFIX)))
        .map(str::to_string);

    Ok(GatewaySubscription {
        id: str_at("/id")
            .ok_or_else(|| PaymentError::transport("subscription without an id"))?
            .to_string(),
        customer_id: match value.get("customer") {
            Some(Value::String(id)) => id.clone(),
            Some(object) => object
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            None => String::new(),
        },
        status: str_at("/status").unwrap_or_default().to_string(),
        item_id: item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        price_id: price
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        plan_code,
        interval: price
            .pointer("/recurring/interval")
            .and_then(Value::as_str)
            .and_then(BillingInterval::from_provider),
        // Item-level since 2025-03-31; subscription-level before.
        current_period_end: unix(item.get("current_period_end"))
            .or_else(|| unix(value.get("current_period_end"))),
        cancel_at_period_end: value
            .get("cancel_at_period_end")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        canceled_at: unix(value.get("canceled_at")),
        ended_at: unix(value.get("ended_at")),
        user_id: str_at("/metadata/user_id").and_then(|id| Uuid::parse_str(id).ok()),
        has_pending_update: value.get("pending_update").is_some_and(|p| !p.is_null()),
    })
}

fn url_of(body: &Value) -> Result<String, PaymentError> {
    body.get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PaymentError::transport("Stripe answered without a URL"))
}

fn id_of(body: &Value) -> Result<String, PaymentError> {
    body.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PaymentError::transport("Stripe answered without an id"))
}

/// Form parameters of a checkout session.
pub fn checkout_params(request: &CheckoutRequest) -> Params {
    let user_id = request.user_id.to_string();
    let mut params = vec![
        param("mode", "subscription"),
        param("customer", &request.customer_id),
        param("client_reference_id", &user_id),
        param("line_items[0][price]", &request.price_id),
        param("line_items[0][quantity]", 1),
        param("success_url", &request.success_url),
        param("cancel_url", &request.cancel_url),
        param("locale", provider_locale(&request.locale)),
        param("metadata[user_id]", &user_id),
        param("metadata[plan_code]", &request.plan_code),
        param("metadata[interval]", request.interval.key()),
        param("subscription_data[metadata][user_id]", &user_id),
        param("subscription_data[metadata][plan_code]", &request.plan_code),
    ];
    if let Some(trial_end) = request.trial_end {
        params.push(param("subscription_data[trial_end]", trial_end));
    }
    if let Some(coupon) = &request.coupon_id {
        params.push(param("discounts[0][coupon]", coupon));
    }
    if let Some(redemption) = request.redemption_id {
        params.push(param("metadata[redemption_id]", redemption));
    }
    params
}

#[async_trait::async_trait]
impl PaymentGateway for StripeGateway {
    async fn create_customer(
        &self,
        user_id: Uuid,
        email: Option<&str>,
        name: &str,
        locale: &str,
    ) -> Result<String, PaymentError> {
        let mut params = vec![
            param("name", name),
            param("metadata[user_id]", user_id),
            param("preferred_locales[0]", provider_locale(locale)),
        ];
        if let Some(email) = email {
            params.push(param("email", email));
        }
        // Two checkouts started at once must not create two customers.
        let key = format!("setlyst-customer-{user_id}");
        let body = self
            .call(Method::POST, "/v1/customers", &params, Some(&key))
            .await?;
        id_of(&body)
    }

    async fn ensure_price(&self, spec: &PriceSpec) -> Result<String, PaymentError> {
        let lookup_key = spec.lookup_key();
        let found = self
            .call(
                Method::GET,
                "/v1/prices",
                &[
                    param("lookup_keys[]", &lookup_key),
                    param("active", "true"),
                    param("limit", 1),
                ],
                None,
            )
            .await?;
        if let Some(id) = found.pointer("/data/0/id").and_then(Value::as_str) {
            return Ok(id.to_string());
        }

        let product = self.ensure_product(spec).await?;
        let body = self
            .call(
                Method::POST,
                "/v1/prices",
                &[
                    param("product", product),
                    param("currency", spec.currency.to_ascii_lowercase()),
                    param("unit_amount", spec.unit_amount),
                    param("recurring[interval]", spec.interval.provider_key()),
                    param("lookup_key", &lookup_key),
                    // A concurrent checkout may have just created the same
                    // price: take the key over instead of failing.
                    param("transfer_lookup_key", "true"),
                    param("nickname", &lookup_key),
                    param("metadata[plan_code]", &spec.plan_code),
                    param("metadata[interval]", spec.interval.key()),
                ],
                None,
            )
            .await?;
        id_of(&body)
    }

    async fn create_coupon(&self, percent: i32, name: &str) -> Result<String, PaymentError> {
        // Valid a little longer than the checkout page it's attached to.
        let redeem_by = Utc::now().timestamp() + 25 * 3600;
        let body = self
            .call(
                Method::POST,
                "/v1/coupons",
                &[
                    param("percent_off", percent.clamp(1, 100)),
                    param("duration", "once"),
                    param("max_redemptions", 1),
                    param("redeem_by", redeem_by),
                    param("name", name.chars().take(40).collect::<String>()),
                ],
                None,
            )
            .await?;
        id_of(&body)
    }

    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<String, PaymentError> {
        let body = self
            .call(
                Method::POST,
                "/v1/checkout/sessions",
                &checkout_params(request),
                None,
            )
            .await?;
        url_of(&body)
    }

    async fn create_portal(
        &self,
        customer_id: &str,
        return_url: &str,
        locale: &str,
    ) -> Result<String, PaymentError> {
        let body = self
            .call(
                Method::POST,
                "/v1/billing_portal/sessions",
                &[
                    param("customer", checked_id(customer_id)?),
                    param("return_url", return_url),
                    param("locale", provider_locale(locale)),
                ],
                None,
            )
            .await?;
        url_of(&body)
    }

    async fn get_subscription(&self, id: &str) -> Result<GatewaySubscription, PaymentError> {
        let path = format!("/v1/subscriptions/{}", checked_id(id)?);
        let body = self.call(Method::GET, &path, &[], None).await?;
        parse_subscription(&body)
    }

    async fn change_price(
        &self,
        subscription_id: &str,
        item_id: &str,
        price_id: &str,
    ) -> Result<GatewaySubscription, PaymentError> {
        let path = format!("/v1/subscriptions/{}", checked_id(subscription_id)?);
        let body = self
            .call(
                Method::POST,
                &path,
                &[
                    param("items[0][id]", checked_id(item_id)?),
                    param("items[0][price]", checked_id(price_id)?),
                    param("proration_behavior", "always_invoice"),
                    param("payment_behavior", "pending_if_incomplete"),
                ],
                None,
            )
            .await?;
        parse_subscription(&body)
    }

    async fn cancel_now(&self, subscription_id: &str) -> Result<(), PaymentError> {
        let current = self.get_subscription(subscription_id).await?;
        if matches!(current.status.as_str(), "canceled" | "incomplete_expired") {
            return Ok(());
        }
        let path = format!("/v1/subscriptions/{}", checked_id(subscription_id)?);
        match self.call(Method::DELETE, &path, &[], None).await {
            Ok(_) => Ok(()),
            Err(e) if e.code.as_deref() == Some("resource_missing") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Trimmed from a real `2025-03-31.basil` subscription.
    fn subscription_json() -> Value {
        json!({
            "id": "sub_1Q", "object": "subscription", "customer": "cus_9",
            "status": "active", "cancel_at_period_end": false,
            "canceled_at": null, "ended_at": null, "pending_update": null,
            "metadata": {"user_id": "0192f3a4-5b6c-7d8e-9f00-112233445566", "plan_code": "pro"},
            "items": {"object": "list", "data": [{
                "id": "si_1", "object": "subscription_item",
                "current_period_start": 1_790_000_000, "current_period_end": 1_792_592_000,
                "price": {
                    "id": "price_1", "object": "price", "product": "setlyst_plan_pro",
                    "recurring": {"interval": "month", "interval_count": 1},
                    "metadata": {"plan_code": "pro", "interval": "monthly"}
                }
            }]}
        })
    }

    #[test]
    fn subscriptions_are_read_from_item_level_periods() {
        let sub = parse_subscription(&subscription_json()).unwrap();
        assert_eq!(sub.id, "sub_1Q");
        assert_eq!(sub.customer_id, "cus_9");
        assert_eq!(sub.status, "active");
        assert_eq!(sub.item_id, "si_1");
        assert_eq!(sub.price_id, "price_1");
        assert_eq!(sub.plan_code.as_deref(), Some("pro"));
        assert_eq!(sub.interval, Some(BillingInterval::Monthly));
        assert_eq!(sub.current_period_end, Some(1_792_592_000));
        assert_eq!(
            sub.user_id.unwrap().to_string(),
            "0192f3a4-5b6c-7d8e-9f00-112233445566"
        );
        assert!(!sub.has_pending_update);
    }

    #[test]
    fn plan_codes_fall_back_to_the_product_id_and_old_periods_still_read() {
        let mut value = subscription_json();
        value["items"]["data"][0]["price"]["metadata"] = json!({});
        value["items"]["data"][0]["price"]["product"] = json!({"id": "setlyst_plan_basic"});
        value["items"]["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("current_period_end");
        value["current_period_end"] = json!(1_700_000_000);
        value["pending_update"] = json!({"expires_at": 1});
        let sub = parse_subscription(&value).unwrap();
        assert_eq!(sub.plan_code.as_deref(), Some("basic"));
        assert_eq!(sub.current_period_end, Some(1_700_000_000));
        assert!(sub.has_pending_update);

        value["items"]["data"] = json!([]);
        assert!(parse_subscription(&value).is_err());
    }

    #[test]
    fn checkout_parameters_carry_price_trial_and_discount() {
        let request = CheckoutRequest {
            user_id: Uuid::nil(),
            customer_id: "cus_9".into(),
            price_id: "price_1".into(),
            plan_code: "pro".into(),
            interval: BillingInterval::Yearly,
            success_url: "https://app/ok?session_id={CHECKOUT_SESSION_ID}".into(),
            cancel_url: "https://app/back".into(),
            locale: "pt-BR".into(),
            trial_end: Some(1_800_000_000),
            coupon_id: Some("co_1".into()),
            redemption_id: Some(Uuid::nil()),
        };
        let params = checkout_params(&request);
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("mode"), Some("subscription"));
        assert_eq!(get("line_items[0][price]"), Some("price_1"));
        assert_eq!(get("locale"), Some("pt-BR"));
        assert_eq!(get("subscription_data[trial_end]"), Some("1800000000"));
        assert_eq!(get("discounts[0][coupon]"), Some("co_1"));
        assert_eq!(get("metadata[interval]"), Some("yearly"));
        assert_eq!(
            get("subscription_data[metadata][user_id]"),
            Some(Uuid::nil().to_string().as_str())
        );

        let plain = checkout_params(&CheckoutRequest {
            trial_end: None,
            coupon_id: None,
            redemption_id: None,
            ..request
        });
        assert!(!plain.iter().any(|(k, _)| k.starts_with("discounts")
            || k == "subscription_data[trial_end]"
            || k == "metadata[redemption_id]"));
    }

    #[test]
    fn ids_are_checked_before_reaching_a_path() {
        assert!(checked_id("sub_1Q2w3E").is_ok());
        assert!(checked_id("../v1/customers").is_err());
        assert!(checked_id("sub_1?expand=x").is_err());
        assert!(checked_id("").is_err());
    }
}
