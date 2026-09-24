//! [`PaymentGateway`] over Stripe's REST API.
//!
//! Requests are form-encoded, as Stripe expects, and pinned to
//! [`API_VERSION`] so response shapes never change under us when the
//! account's default version is upgraded. Only the fields the API uses are
//! read from responses.

use super::{
    CheckoutRequest, CheckoutSession, GatewayInvoice, GatewayRefund, GatewaySubscription,
    InvoicePage, PaymentError, PaymentFees, PaymentGateway, PriceSpec, RefundPage,
    SubscriptionPage, provider_locale,
};
use crate::{config::StripeConfig, models::billing::BillingInterval};
use chrono::Utc;
use rand::Rng;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};
use uuid::Uuid;

/// The Stripe API version every request is made with. Since this version
/// billing periods live on subscription items.
pub const API_VERSION: &str = "2025-03-31.basil";

/// Products are created with this id prefix plus the plan code.
pub const PRODUCT_PREFIX: &str = "setlyst_plan_";

/// Longest wait for one Stripe call. Webhooks and checkouts make a few
/// calls in a row and must finish well inside the server's 30 s limit.
const CALL_TIMEOUT_SECS: u64 = 10;

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
            .timeout(Duration::from_secs(CALL_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("setlyst-api/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            api_base: config.api_base.clone(),
            secret_key: config.secret_key.clone(),
        })
    }

    /// One Stripe request. Reads and keyed writes (safe to repeat) are
    /// retried once, after a short random pause, when Stripe can't be
    /// reached, rate limits or fails on its side.
    async fn call(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        idempotency_key: Option<&str>,
    ) -> Result<Value, PaymentError> {
        let repeatable = method == Method::GET || idempotency_key.is_some();
        match self
            .call_once(method.clone(), path, params, idempotency_key)
            .await
        {
            Err(e) if repeatable && is_transient(&e) => {
                let pause = rand::thread_rng().gen_range(200..700);
                warn!(%method, path, error = %e, "Stripe call failed; retrying once");
                tokio::time::sleep(Duration::from_millis(pause)).await;
                self.call_once(method, path, params, idempotency_key).await
            }
            other => other,
        }
    }

    async fn call_once(
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
        let key = format!("setlyst-product-{id}");
        let created = self
            .call(
                Method::POST,
                "/v1/products",
                &[
                    param("id", &id),
                    param("name", &spec.product_name),
                    param("metadata[plan_code]", &spec.plan_code),
                ],
                Some(&key),
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

/// Worth one more try: no answer, rate limited, or Stripe's own failure.
fn is_transient(e: &PaymentError) -> bool {
    match e.status {
        None => true,
        Some(status) => status == 429 || status >= 500,
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
        trial_end: unix(value.get("trial_end")),
        unit_amount: price.get("unit_amount").and_then(Value::as_i64),
        currency: price
            .get("currency")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase),
        latest_invoice_id: id_or_object(value.get("latest_invoice")),
        latest_invoice_status: value
            .pointer("/latest_invoice/status")
            .and_then(Value::as_str)
            .map(str::to_string),
        hosted_invoice_url: value
            .pointer("/latest_invoice/hosted_invoice_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        latest_payment_status: value
            .pointer("/latest_invoice/payments/data")
            .and_then(Value::as_array)
            .and_then(|payments| {
                payments.iter().find_map(|p| {
                    p.pointer("/payment/payment_intent/status")
                        .and_then(Value::as_str)
                })
            })
            .map(str::to_string),
    })
}

/// Reads a refund object.
pub fn parse_refund(value: &Value) -> Option<GatewayRefund> {
    Some(GatewayRefund {
        id: value.get("id")?.as_str()?.to_string(),
        payment_intent_id: id_or_object(value.get("payment_intent")),
        charge_id: id_or_object(value.get("charge")),
        amount: value.get("amount").and_then(Value::as_i64).unwrap_or(0),
        currency: value
            .get("currency")
            .and_then(Value::as_str)
            .map(str::to_ascii_uppercase),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        created: unix(value.get("created")),
    })
}

fn last_id(data: Option<&Vec<Value>>) -> Option<String> {
    data.and_then(|d| d.last())
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn has_more(body: &Value, data: Option<&Vec<Value>>) -> bool {
    body.get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && data.is_some_and(|d| !d.is_empty())
}

/// A longer billing period than this is a yearly charge.
const YEARLY_PERIOD_SECS: i64 = 60 * 86_400;

fn id_or_object(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(object @ Value::Object(_)) => object
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Reads a paid invoice. `None` for one that isn't paid or paid nothing
/// (a trial's zero invoice).
pub fn parse_invoice(value: &Value) -> Option<GatewayInvoice> {
    if value.get("status").and_then(Value::as_str) != Some("paid") {
        return None;
    }
    let amount_paid = value.get("amount_paid").and_then(Value::as_i64)?;
    if amount_paid <= 0 {
        return None;
    }
    let id = value.get("id").and_then(Value::as_str)?.to_string();

    // The subscription line that was charged the most (a plan change bills
    // a credit line for the old plan next to the new one).
    let line = value
        .pointer("/lines/data")
        .and_then(Value::as_array)
        .and_then(|lines| {
            lines
                .iter()
                .max_by_key(|l| l.get("amount").and_then(Value::as_i64).unwrap_or(i64::MIN))
        });
    let product = line.and_then(|l| {
        id_or_object(l.pointer("/pricing/price_details/product"))
            .or_else(|| id_or_object(l.pointer("/price/product")))
            .or_else(|| id_or_object(l.pointer("/plan/product")))
    });
    let plan_code = line
        .and_then(|l| {
            l.pointer("/price/metadata/plan_code")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            product
                .as_deref()
                .and_then(|id| id.strip_prefix(PRODUCT_PREFIX))
                .map(str::to_string)
        });
    let interval = line
        .and_then(|l| {
            l.pointer("/price/recurring/interval")
                .and_then(Value::as_str)
                .and_then(BillingInterval::from_provider)
        })
        .or_else(|| {
            let start = unix(line?.pointer("/period/start"))?;
            let end = unix(line?.pointer("/period/end"))?;
            Some(if end - start > YEARLY_PERIOD_SECS {
                BillingInterval::Yearly
            } else {
                BillingInterval::Monthly
            })
        });

    let payment_intent_id = value
        .pointer("/payments/data")
        .and_then(Value::as_array)
        .and_then(|payments| {
            payments
                .iter()
                .find_map(|p| id_or_object(p.pointer("/payment/payment_intent")))
        })
        .or_else(|| id_or_object(value.get("payment_intent")));

    Some(GatewayInvoice {
        id,
        customer_id: id_or_object(value.get("customer")),
        subscription_id: id_or_object(value.pointer("/parent/subscription_details/subscription"))
            .or_else(|| id_or_object(value.get("subscription"))),
        user_id: value
            .pointer("/parent/subscription_details/metadata/user_id")
            .or_else(|| value.pointer("/subscription_details/metadata/user_id"))
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok()),
        payment_intent_id,
        plan_code,
        interval,
        amount_paid,
        currency: value
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("brl")
            .to_ascii_uppercase(),
        paid_at: unix(value.pointer("/status_transitions/paid_at"))
            .or_else(|| unix(value.get("created"))),
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
        // Cards only: delayed methods (boleto, Pix) would start the
        // subscription unpaid and need their own handling.
        param("payment_method_types[0]", "card"),
    ];
    if let Some(message) = &request.terms_message {
        // The buyer must tick "I accept the Terms of Service" (the URL set
        // in the Dashboard's public details); the text links the
        // Subscription Terms and states renewal, cancellation and the
        // 7-day withdrawal right.
        params.push(param("consent_collection[terms_of_service]", "required"));
        params.push(param(
            "custom_text[terms_of_service_acceptance][message]",
            message,
        ));
    }
    if let Some(version) = &request.terms_version {
        params.push(param("metadata[terms_version]", version));
    }
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
        generation: i32,
    ) -> Result<String, PaymentError> {
        let mut params = vec![
            param("name", name),
            param("metadata[user_id]", user_id),
            param("preferred_locales[0]", provider_locale(locale)),
        ];
        if let Some(email) = email {
            params.push(param("email", email));
        }
        // Two checkouts started at once must not create two customers; a
        // new generation (the old customer was deleted) must.
        let key = format!("setlyst-customer-{user_id}-{generation}");
        let body = self
            .call(Method::POST, "/v1/customers", &params, Some(&key))
            .await?;
        id_of(&body)
    }

    async fn update_customer(
        &self,
        customer_id: &str,
        email: Option<&str>,
        name: &str,
    ) -> Result<(), PaymentError> {
        let path = format!("/v1/customers/{}", checked_id(customer_id)?);
        // An empty value clears the e-mail at Stripe.
        let params = vec![param("name", name), param("email", email.unwrap_or(""))];
        self.call(Method::POST, &path, &params, None).await?;
        Ok(())
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
                Some(&format!("setlyst-price-{lookup_key}")),
            )
            .await?;
        id_of(&body)
    }

    async fn create_coupon(
        &self,
        percent: i32,
        name: &str,
        idempotency_key: &str,
    ) -> Result<String, PaymentError> {
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
                Some(idempotency_key),
            )
            .await?;
        id_of(&body)
    }

    async fn create_checkout(
        &self,
        request: &CheckoutRequest,
    ) -> Result<CheckoutSession, PaymentError> {
        let body = self
            .call(
                Method::POST,
                "/v1/checkout/sessions",
                &checkout_params(request),
                Some(&request.idempotency_key),
            )
            .await?;
        Ok(CheckoutSession {
            id: id_of(&body)?,
            url: url_of(&body)?,
        })
    }

    async fn expire_checkout(&self, session_id: &str) -> Result<(), PaymentError> {
        let path = format!("/v1/checkout/sessions/{}/expire", checked_id(session_id)?);
        match self.call(Method::POST, &path, &[], None).await {
            Ok(_) => Ok(()),
            // Already completed or expired (Stripe answers 400), or gone.
            Err(e) if e.status == Some(400) || e.status == Some(404) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn list_subscriptions(
        &self,
        customer_id: Option<&str>,
        starting_after: Option<&str>,
    ) -> Result<SubscriptionPage, PaymentError> {
        let mut params = vec![param("status", "all"), param("limit", 100)];
        if let Some(customer) = customer_id {
            params.push(param("customer", checked_id(customer)?));
        }
        if let Some(after) = starting_after {
            params.push(param("starting_after", checked_id(after)?));
        }
        let body = self
            .call(Method::GET, "/v1/subscriptions", &params, None)
            .await?;
        let data = body.get("data").and_then(Value::as_array);
        Ok(SubscriptionPage {
            subscriptions: data
                .map(|subs| {
                    subs.iter()
                        .filter_map(|s| parse_subscription(s).ok())
                        .collect()
                })
                .unwrap_or_default(),
            last_id: last_id(data),
            has_more: has_more(&body, data),
        })
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
                    // The proration invoice and its payment: a charge that
                    // needs 3-D Secure leaves it open with a hosted page.
                    param(
                        "expand[]",
                        "latest_invoice.payments.data.payment.payment_intent",
                    ),
                ],
                Some(&format!(
                    "setlyst-change-{subscription_id}-{price_id}-{item_id}"
                )),
            )
            .await?;
        parse_subscription(&body)
    }

    async fn get_paid_invoice(&self, id: &str) -> Result<Option<GatewayInvoice>, PaymentError> {
        let path = format!("/v1/invoices/{}", checked_id(id)?);
        let body = self
            .call(Method::GET, &path, &[param("expand[]", "payments")], None)
            .await?;
        Ok(parse_invoice(&body))
    }

    async fn list_subscription_invoices(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<GatewayInvoice>, PaymentError> {
        let body = self
            .call(
                Method::GET,
                "/v1/invoices",
                &[
                    param("subscription", checked_id(subscription_id)?),
                    param("status", "paid"),
                    param("limit", 100),
                    param("expand[]", "data.payments"),
                ],
                None,
            )
            .await?;
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .map(|invoices| invoices.iter().filter_map(parse_invoice).collect())
            .unwrap_or_default())
    }

    async fn refund(
        &self,
        payment_intent_id: &str,
        amount: Option<i64>,
        idempotency_key: &str,
    ) -> Result<GatewayRefund, PaymentError> {
        let mut params = vec![
            param("payment_intent", checked_id(payment_intent_id)?),
            param("reason", "requested_by_customer"),
        ];
        if let Some(amount) = amount {
            params.push(param("amount", amount));
        }
        let body = self
            .call(Method::POST, "/v1/refunds", &params, Some(idempotency_key))
            .await?;
        parse_refund(&body).ok_or_else(|| PaymentError::transport("unreadable Stripe refund"))
    }

    async fn list_refunds_for_payment(
        &self,
        payment_intent_id: &str,
    ) -> Result<Vec<GatewayRefund>, PaymentError> {
        let body = self
            .call(
                Method::GET,
                "/v1/refunds",
                &[
                    param("payment_intent", checked_id(payment_intent_id)?),
                    param("limit", 100),
                ],
                None,
            )
            .await?;
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .map(|refunds| refunds.iter().filter_map(parse_refund).collect())
            .unwrap_or_default())
    }

    async fn payment_fees(
        &self,
        payment_intent_id: &str,
    ) -> Result<Option<PaymentFees>, PaymentError> {
        let path = format!("/v1/payment_intents/{}", checked_id(payment_intent_id)?);
        let body = self
            .call(
                Method::GET,
                &path,
                &[param("expand[]", "latest_charge.balance_transaction")],
                None,
            )
            .await?;
        let transaction = body.pointer("/latest_charge/balance_transaction");
        let (Some(fee), Some(net)) = (
            transaction
                .and_then(|t| t.get("fee"))
                .and_then(Value::as_i64),
            transaction
                .and_then(|t| t.get("net"))
                .and_then(Value::as_i64),
        ) else {
            return Ok(None);
        };
        Ok(Some(PaymentFees {
            charge_id: id_or_object(body.get("latest_charge")),
            fee,
            net,
        }))
    }

    async fn probe_permissions(&self) -> Vec<(String, PaymentError)> {
        // Read-only probes of what the webhook and the finance report
        // read; write scopes can't be checked without side effects.
        let probes = [
            ("Invoices (read)", "/v1/invoices"),
            ("Refunds (read)", "/v1/refunds"),
            ("Charges (read)", "/v1/charges"),
            ("Disputes (read)", "/v1/disputes"),
            ("Subscriptions (read)", "/v1/subscriptions"),
            ("Customers (read)", "/v1/customers"),
            ("Checkout Sessions (read)", "/v1/checkout/sessions"),
            ("Prices (read)", "/v1/prices"),
        ];
        let mut failed = Vec::new();
        for (scope, path) in probes {
            if let Err(e) = self
                .call(Method::GET, path, &[param("limit", 1)], None)
                .await
            {
                failed.push((scope.to_string(), e));
            }
        }
        failed
    }

    async fn list_paid_invoices(
        &self,
        starting_after: Option<&str>,
    ) -> Result<InvoicePage, PaymentError> {
        let mut params = vec![
            param("status", "paid"),
            param("limit", 100),
            param("expand[]", "data.payments"),
        ];
        if let Some(after) = starting_after {
            params.push(param("starting_after", checked_id(after)?));
        }
        let body = self
            .call(Method::GET, "/v1/invoices", &params, None)
            .await?;
        let data = body.get("data").and_then(Value::as_array);
        Ok(InvoicePage {
            invoices: data
                .map(|invoices| invoices.iter().filter_map(parse_invoice).collect())
                .unwrap_or_default(),
            last_id: data
                .and_then(|d| d.last())
                .and_then(|i| i.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            has_more: body
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && data.is_some_and(|d| !d.is_empty()),
        })
    }

    async fn invoice_for_payment(
        &self,
        payment_intent_id: &str,
    ) -> Result<Option<String>, PaymentError> {
        let body = self
            .call(
                Method::GET,
                "/v1/invoice_payments",
                &[
                    param("payment[type]", "payment_intent"),
                    param("payment[payment_intent]", checked_id(payment_intent_id)?),
                    param("limit", 1),
                ],
                None,
            )
            .await?;
        Ok(id_or_object(body.pointer("/data/0/invoice")))
    }

    async fn list_refunds(&self, starting_after: Option<&str>) -> Result<RefundPage, PaymentError> {
        let mut params = vec![param("limit", 100)];
        if let Some(after) = starting_after {
            params.push(param("starting_after", checked_id(after)?));
        }
        let body = self.call(Method::GET, "/v1/refunds", &params, None).await?;
        let data = body.get("data").and_then(Value::as_array);
        Ok(RefundPage {
            refunds: data
                .map(|refunds| refunds.iter().filter_map(parse_refund).collect())
                .unwrap_or_default(),
            last_id: last_id(data),
            has_more: has_more(&body, data),
        })
    }

    async fn delete_customer(&self, customer_id: &str) -> Result<(), PaymentError> {
        let path = format!("/v1/customers/{}", checked_id(customer_id)?);
        match self.call(Method::DELETE, &path, &[], None).await {
            Ok(_) => Ok(()),
            Err(e) if e.code.as_deref() == Some("resource_missing") => Ok(()),
            Err(e) => Err(e),
        }
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
            terms_message: Some("Li e aceito os Termos de Assinatura.".into()),
            terms_version: Some("2026-09-01".into()),
            idempotency_key: "k".into(),
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
        // Cards only, and the Subscription Terms must be accepted.
        assert_eq!(get("payment_method_types[0]"), Some("card"));
        assert_eq!(
            get("consent_collection[terms_of_service]"),
            Some("required")
        );
        assert_eq!(
            get("custom_text[terms_of_service_acceptance][message]"),
            Some("Li e aceito os Termos de Assinatura.")
        );
        assert_eq!(get("metadata[terms_version]"), Some("2026-09-01"));

        let plain = checkout_params(&CheckoutRequest {
            trial_end: None,
            coupon_id: None,
            redemption_id: None,
            terms_message: None,
            terms_version: None,
            ..request
        });
        assert!(!plain.iter().any(|(k, _)| k.starts_with("discounts")
            || k.starts_with("consent_collection")
            || k == "subscription_data[trial_end]"
            || k == "metadata[redemption_id]"));
    }

    #[test]
    fn expanded_invoices_and_prices_are_read_from_subscriptions() {
        let mut value = subscription_json();
        value["trial_end"] = json!(1_795_000_000);
        value["items"]["data"][0]["price"]["unit_amount"] = json!(3990);
        value["items"]["data"][0]["price"]["currency"] = json!("brl");
        value["latest_invoice"] = json!({
            "id": "in_9", "status": "open",
            "hosted_invoice_url": "https://invoice.stripe.com/i/acct/in_9",
            "payments": {"data": [{"payment": {"payment_intent": {
                "id": "pi_9", "status": "requires_action"
            }}}]}
        });
        let sub = parse_subscription(&value).unwrap();
        assert_eq!(sub.trial_end, Some(1_795_000_000));
        assert_eq!(sub.unit_amount, Some(3990));
        assert_eq!(sub.currency.as_deref(), Some("BRL"));
        assert_eq!(sub.latest_invoice_id.as_deref(), Some("in_9"));
        assert_eq!(sub.latest_invoice_status.as_deref(), Some("open"));
        assert_eq!(
            sub.latest_payment_status.as_deref(),
            Some("requires_action")
        );
        assert!(
            sub.hosted_invoice_url
                .as_deref()
                .unwrap()
                .starts_with("https://")
        );
        assert!(sub.may_charge());

        value["latest_invoice"] = json!("in_10");
        value["status"] = json!("canceled");
        let sub = parse_subscription(&value).unwrap();
        assert_eq!(sub.latest_invoice_id.as_deref(), Some("in_10"));
        assert_eq!(sub.latest_invoice_status, None);
        assert!(!sub.may_charge());
    }

    #[test]
    fn refunds_are_read_with_their_date_and_charge() {
        let refund = parse_refund(&json!({
            "id": "re_1", "object": "refund", "amount": 1990, "currency": "brl",
            "payment_intent": "pi_1", "charge": {"id": "ch_1"},
            "status": "succeeded", "created": 1_700_000_000
        }))
        .unwrap();
        assert_eq!(refund.charge_id.as_deref(), Some("ch_1"));
        assert_eq!(refund.currency.as_deref(), Some("BRL"));
        assert_eq!(refund.created, Some(1_700_000_000));
        assert!(refund.counts());
        let failed = GatewayRefund {
            status: "failed".into(),
            ..refund
        };
        assert!(!failed.counts());
    }

    #[test]
    fn only_transient_failures_are_retried() {
        let error = |status: Option<u16>| PaymentError {
            status,
            code: None,
            message: String::new(),
        };
        assert!(is_transient(&error(None)));
        assert!(is_transient(&error(Some(429))));
        assert!(is_transient(&error(Some(503))));
        assert!(!is_transient(&error(Some(400))));
        assert!(!is_transient(&error(Some(402))));
    }

    #[test]
    fn ids_are_checked_before_reaching_a_path() {
        assert!(checked_id("sub_1Q2w3E").is_ok());
        assert!(checked_id("../v1/customers").is_err());
        assert!(checked_id("sub_1?expand=x").is_err());
        assert!(checked_id("").is_err());
    }

    #[test]
    fn paid_invoices_are_read_from_basil_shapes() {
        let user = Uuid::now_v7();
        let body = json!({
            "id": "in_1", "object": "invoice", "status": "paid",
            "amount_paid": 2990, "currency": "brl", "customer": "cus_1",
            "created": 1_700_000_000,
            "status_transitions": {"paid_at": 1_700_000_100},
            "parent": {"subscription_details": {
                "subscription": "sub_1", "metadata": {"user_id": user.to_string()}
            }},
            "payments": {"data": [{"payment": {"type": "payment_intent", "payment_intent": "pi_1"}}]},
            "lines": {"data": [
                {"amount": -1000, "pricing": {"price_details": {"product": "setlyst_plan_basic"}},
                 "period": {"start": 1_700_000_000, "end": 1_702_592_000}},
                {"amount": 3990, "pricing": {"price_details": {"product": "setlyst_plan_pro"}},
                 "period": {"start": 1_700_000_000, "end": 1_731_536_000}}
            ]}
        });
        let invoice = parse_invoice(&body).unwrap();
        assert_eq!(invoice.id, "in_1");
        assert_eq!(invoice.amount_paid, 2990);
        assert_eq!(invoice.currency, "BRL");
        assert_eq!(invoice.customer_id.as_deref(), Some("cus_1"));
        assert_eq!(invoice.subscription_id.as_deref(), Some("sub_1"));
        assert_eq!(invoice.user_id, Some(user));
        assert_eq!(invoice.payment_intent_id.as_deref(), Some("pi_1"));
        // The line actually charged, a year long.
        assert_eq!(invoice.plan_code.as_deref(), Some("pro"));
        assert_eq!(invoice.interval, Some(BillingInterval::Yearly));
        assert_eq!(invoice.paid_at, Some(1_700_000_100));

        // Older shapes still read.
        let legacy = json!({
            "id": "in_2", "status": "paid", "amount_paid": 100, "currency": "usd",
            "subscription": "sub_2", "payment_intent": "pi_2",
            "lines": {"data": [{"amount": 100, "price": {
                "product": "setlyst_plan_intermediate", "recurring": {"interval": "month"}
            }}]}
        });
        let legacy = parse_invoice(&legacy).unwrap();
        assert_eq!(legacy.subscription_id.as_deref(), Some("sub_2"));
        assert_eq!(legacy.payment_intent_id.as_deref(), Some("pi_2"));
        assert_eq!(legacy.plan_code.as_deref(), Some("intermediate"));
        assert_eq!(legacy.interval, Some(BillingInterval::Monthly));

        // Unpaid and zero invoices (a trial's) are not payments.
        assert!(
            parse_invoice(&json!({"id": "in_3", "status": "open", "amount_paid": 0})).is_none()
        );
        assert!(
            parse_invoice(&json!({"id": "in_4", "status": "paid", "amount_paid": 0})).is_none()
        );
    }
}
