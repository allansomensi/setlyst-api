//! Paid subscriptions: checkout, the Stripe webhook, plan changes, the
//! billing portal and cancellation. Stripe is replaced by [`FakeGateway`];
//! webhook bodies are signed exactly like Stripe signs them.

mod common;

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use chrono::Utc;
use common::{STRONG_PASSWORD, TestApp};
use serde_json::{Value, json};
use setlyst_api::{
    models::{billing::BillingInterval, user::Role},
    payments::{
        CheckoutRequest, CheckoutSession, GatewayInvoice, GatewayRefund, GatewaySubscription,
        InvoicePage, PaymentError, PaymentGateway, Payments, PriceSpec, RefundPage,
        SubscriptionPage, webhook::sign,
    },
    routes::api_router,
    services::billing::run_subscription_maintenance,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "whsec_integration_tests";

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Stripe, in memory. Prices are named after their lookup key so a
/// subscription moved to a price knows its plan and interval.
#[derive(Default)]
struct FakeGateway {
    calls: Mutex<Vec<String>>,
    checkouts: Mutex<Vec<CheckoutRequest>>,
    subscriptions: Mutex<HashMap<String, GatewaySubscription>>,
    decline_changes: Mutex<bool>,
    customers: Mutex<u32>,
    /// Paid invoices, newest first.
    invoices: Mutex<Vec<GatewayInvoice>>,
    /// Refunds, newest first.
    refunds: Mutex<Vec<GatewayRefund>>,
    /// Refunds made through the API: `(payment intent, idempotency key)`.
    refunded: Mutex<Vec<(String, String)>>,
    /// `get_subscription` fails as if Stripe were down.
    unreachable: Mutex<bool>,
}

impl FakeGateway {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }

    fn calls_to(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }

    fn last_checkout(&self) -> CheckoutRequest {
        self.checkouts.lock().unwrap().last().unwrap().clone()
    }

    fn put(&self, subscription: GatewaySubscription) {
        self.subscriptions
            .lock()
            .unwrap()
            .insert(subscription.id.clone(), subscription);
    }

    fn edit(&self, id: &str, change: impl FnOnce(&mut GatewaySubscription)) {
        change(self.subscriptions.lock().unwrap().get_mut(id).unwrap());
    }

    fn get(&self, id: &str) -> GatewaySubscription {
        self.subscriptions.lock().unwrap()[id].clone()
    }
}

#[async_trait::async_trait]
impl PaymentGateway for FakeGateway {
    async fn create_customer(
        &self,
        user_id: Uuid,
        email: Option<&str>,
        _name: &str,
        _locale: &str,
        _generation: i32,
    ) -> Result<String, PaymentError> {
        let mut count = self.customers.lock().unwrap();
        *count += 1;
        self.record(format!("customer:{user_id}:{}", email.unwrap_or("-")));
        Ok(format!("cus_{count}"))
    }

    async fn ensure_price(&self, spec: &PriceSpec) -> Result<String, PaymentError> {
        self.record(format!("price:{}", spec.lookup_key()));
        Ok(format!("price_{}", spec.lookup_key()))
    }

    async fn update_customer(
        &self,
        customer_id: &str,
        email: Option<&str>,
        _name: &str,
    ) -> Result<(), PaymentError> {
        self.record(format!(
            "update_customer:{customer_id}:{}",
            email.unwrap_or("-")
        ));
        Ok(())
    }

    async fn create_coupon(
        &self,
        percent: i32,
        _name: &str,
        _idempotency_key: &str,
    ) -> Result<String, PaymentError> {
        self.record(format!("coupon:{percent}"));
        Ok(format!("co_{percent}"))
    }

    async fn create_checkout(
        &self,
        request: &CheckoutRequest,
    ) -> Result<CheckoutSession, PaymentError> {
        self.record("checkout");
        let mut checkouts = self.checkouts.lock().unwrap();
        checkouts.push(request.clone());
        let id = format!("cs_{}", checkouts.len());
        Ok(CheckoutSession {
            url: format!("https://checkout.stripe.test/c/pay/{id}"),
            id,
        })
    }

    async fn expire_checkout(&self, session_id: &str) -> Result<(), PaymentError> {
        self.record(format!("expire:{session_id}"));
        Ok(())
    }

    async fn list_subscriptions(
        &self,
        customer_id: Option<&str>,
        _starting_after: Option<&str>,
    ) -> Result<SubscriptionPage, PaymentError> {
        let subscriptions: Vec<GatewaySubscription> = self
            .subscriptions
            .lock()
            .unwrap()
            .values()
            .filter(|s| customer_id.is_none_or(|c| s.customer_id == c))
            .cloned()
            .collect();
        Ok(SubscriptionPage {
            last_id: subscriptions.last().map(|s| s.id.clone()),
            subscriptions,
            has_more: false,
        })
    }

    async fn create_portal(
        &self,
        customer_id: &str,
        return_url: &str,
        _locale: &str,
    ) -> Result<String, PaymentError> {
        self.record(format!("portal:{customer_id}:{return_url}"));
        Ok("https://billing.stripe.test/p/session/1".into())
    }

    async fn get_subscription(&self, id: &str) -> Result<GatewaySubscription, PaymentError> {
        if *self.unreachable.lock().unwrap() {
            return Err(PaymentError::transport("Stripe unreachable"));
        }
        self.subscriptions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| PaymentError {
                status: Some(404),
                code: Some("resource_missing".into()),
                message: format!("No such subscription: '{id}'"),
            })
    }

    async fn change_price(
        &self,
        subscription_id: &str,
        _item_id: &str,
        price_id: &str,
    ) -> Result<GatewaySubscription, PaymentError> {
        self.record(format!("change:{subscription_id}:{price_id}"));
        let mut subscriptions = self.subscriptions.lock().unwrap();
        let subscription = subscriptions.get_mut(subscription_id).unwrap();
        if *self.decline_changes.lock().unwrap() {
            let mut declined = subscription.clone();
            declined.has_pending_update = true;
            return Ok(declined);
        }
        // price_setlyst_<plan>_<month|year>_<amount>_<currency>
        let parts: Vec<&str> = price_id.split('_').collect();
        subscription.price_id = price_id.to_string();
        subscription.plan_code = Some(parts[2].to_string());
        subscription.interval = BillingInterval::from_provider(parts[3]);
        Ok(subscription.clone())
    }

    async fn get_paid_invoice(&self, id: &str) -> Result<Option<GatewayInvoice>, PaymentError> {
        Ok(self
            .invoices
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .cloned())
    }

    /// Two per page, so the sync has to follow the cursor.
    async fn list_paid_invoices(
        &self,
        starting_after: Option<&str>,
    ) -> Result<InvoicePage, PaymentError> {
        let invoices = self.invoices.lock().unwrap().clone();
        let start = starting_after
            .and_then(|after| invoices.iter().position(|i| i.id == after))
            .map_or(0, |p| p + 1);
        let page: Vec<GatewayInvoice> = invoices.iter().skip(start).take(2).cloned().collect();
        Ok(InvoicePage {
            has_more: start + page.len() < invoices.len(),
            last_id: page.last().map(|i| i.id.clone()),
            invoices: page,
        })
    }

    async fn list_subscription_invoices(
        &self,
        subscription_id: &str,
    ) -> Result<Vec<GatewayInvoice>, PaymentError> {
        Ok(self
            .invoices
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.subscription_id.as_deref() == Some(subscription_id))
            .cloned()
            .collect())
    }

    async fn refund(
        &self,
        payment_intent_id: &str,
        amount: Option<i64>,
        idempotency_key: &str,
    ) -> Result<GatewayRefund, PaymentError> {
        self.record(format!("refund:{payment_intent_id}:{idempotency_key}"));
        self.refunded
            .lock()
            .unwrap()
            .push((payment_intent_id.into(), idempotency_key.into()));
        let paid = self
            .invoices
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.payment_intent_id.as_deref() == Some(payment_intent_id))
            .map_or(0, |i| i.amount_paid);
        Ok(GatewayRefund {
            id: format!("re_{idempotency_key}"),
            payment_intent_id: Some(payment_intent_id.into()),
            amount: amount.unwrap_or(paid),
            currency: Some("BRL".into()),
            status: "succeeded".into(),
            created: Some(Utc::now().timestamp()),
            ..Default::default()
        })
    }

    async fn invoice_for_payment(
        &self,
        payment_intent_id: &str,
    ) -> Result<Option<String>, PaymentError> {
        Ok(self
            .invoices
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.payment_intent_id.as_deref() == Some(payment_intent_id))
            .map(|i| i.id.clone()))
    }

    async fn list_refunds(
        &self,
        _starting_after: Option<&str>,
    ) -> Result<RefundPage, PaymentError> {
        let refunds = self.refunds.lock().unwrap().clone();
        Ok(RefundPage {
            last_id: refunds.last().map(|r| r.id.clone()),
            refunds,
            has_more: false,
        })
    }

    async fn delete_customer(&self, customer_id: &str) -> Result<(), PaymentError> {
        self.record(format!("delete_customer:{customer_id}"));
        Ok(())
    }

    async fn cancel_now(&self, subscription_id: &str) -> Result<(), PaymentError> {
        self.record(format!("cancel:{subscription_id}"));
        if let Some(subscription) = self.subscriptions.lock().unwrap().get_mut(subscription_id) {
            subscription.status = "canceled".into();
            subscription.ended_at = Some(Utc::now().timestamp());
        }
        Ok(())
    }
}

fn subscription(id: &str, user_id: Uuid, plan: &str, days: i64) -> GatewaySubscription {
    GatewaySubscription {
        id: id.into(),
        customer_id: format!("cus_{}", user_id.simple()),
        status: "active".into(),
        item_id: format!("si_{id}"),
        price_id: format!("price_setlyst_{plan}_month_0_brl"),
        plan_code: Some(plan.into()),
        interval: Some(BillingInterval::Monthly),
        current_period_end: Some(Utc::now().timestamp() + days * 86_400),
        cancel_at_period_end: false,
        canceled_at: None,
        ended_at: None,
        user_id: Some(user_id),
        ..Default::default()
    }
}

/// The app with the fake gateway plugged in.
fn with_payments(app: &mut TestApp) -> Arc<FakeGateway> {
    let fake = Arc::new(FakeGateway::default());
    app.state = app.state.clone().with_payments(Some(Payments {
        gateway: fake.clone(),
        webhook_secret: WEBHOOK_SECRET.into(),
        livemode: false,
    }));
    app.router = api_router(app.state.clone());
    fake
}

/// Posts a raw webhook body with an optional `Stripe-Signature`.
async fn deliver(app: &TestApp, body: &Value, signature: Option<String>) -> (StatusCode, Value) {
    let bytes = body.to_string().into_bytes();
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/webhooks/stripe")
        .header("content-type", "application/json");
    if let Some(signature) = signature {
        request = request.header("stripe-signature", signature);
    }
    let response = app
        .router
        .clone()
        .oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Delivers a correctly signed event about `subscription_id`.
async fn event(app: &TestApp, id: &str, kind: &str, subscription_id: &str) -> StatusCode {
    let body = json!({
        "id": id, "object": "event", "type": kind,
        "data": {"object": {"object": "subscription", "id": subscription_id}}
    });
    let signature = sign(
        body.to_string().as_bytes(),
        WEBHOOK_SECRET,
        Utc::now().timestamp(),
    );
    let (status, reply) = deliver(app, &body, Some(signature)).await;
    assert!(
        status == StatusCode::OK || status.is_server_error(),
        "{status}: {reply}"
    );
    status
}

async fn verified_user(app: &TestApp, username: &str) -> (Uuid, String) {
    let email = format!("{username}@example.com");
    let (id, token) = app.registered_user(username, &email).await;
    let verified = app.verify_email(&token, &email).await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    (id, token)
}

async fn subscription_row(app: &TestApp, user_id: Uuid) -> Value {
    let row: Option<Value> =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM subscriptions s WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&app.pool)
            .await
            .unwrap();
    row.unwrap_or(Value::Null)
}

async fn event_kinds(app: &TestApp, user_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT kind FROM subscription_events WHERE user_id = $1 ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .unwrap()
}

fn checkout_body(plan: &str, interval: &str) -> Value {
    json!({ "plan_code": plan, "interval": interval })
}

#[tokio::test]
async fn checkout_needs_payments_enforcement_a_real_plan_and_a_verified_email() {
    let mut app = app!();
    let (_, token) = verified_user(&app, "early.bird").await;

    // Not configured.
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["payments_enabled"], false);
    let unavailable = app
        .post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unavailable.code(), "PAYMENTS_UNAVAILABLE");

    let fake = with_payments(&mut app);
    assert_eq!(
        app.get("/billing/me", &token).await.body["payments_enabled"],
        true
    );

    // Nothing is charged during the pre-release.
    let free = app
        .post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    assert_eq!(free.code(), "BILLING_NOT_ENFORCED");

    app.enforce_billing().await;
    let unknown = app
        .post(
            "/billing/checkout",
            &token,
            checkout_body("gold", "monthly"),
        )
        .await;
    assert_eq!(unknown.code(), "PLAN_NOT_FOUND");
    sqlx::query("UPDATE plans SET price_yearly_cents = 0 WHERE code = 'basic'")
        .execute(&app.pool)
        .await
        .unwrap();
    let no_price = app
        .post(
            "/billing/checkout",
            &token,
            checkout_body("basic", "yearly"),
        )
        .await;
    assert_eq!(no_price.code(), "PLAN_NOT_PURCHASABLE");
    let bad_interval = app
        .post("/billing/checkout", &token, checkout_body("pro", "weekly"))
        .await;
    assert!(bad_interval.status.is_client_error());

    // Receipts go to the account's address: it must be confirmed.
    let (_, unverified) = app
        .registered_user("no.proof", "no.proof@example.com")
        .await;
    let refused = app
        .post(
            "/billing/checkout",
            &unverified,
            checkout_body("pro", "monthly"),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.code(), "EMAIL_NOT_VERIFIED");
    assert_eq!(fake.calls_to("checkout"), 0, "nothing reached Stripe");
}

#[tokio::test]
async fn checkout_charges_database_prices_and_carries_trials_and_discounts() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.set_billing(json!({ "enforced": true, "trial_plan": "pro", "trial_days": 30 }))
        .await;
    let (user_id, token) = verified_user(&app, "gigging.anna").await;
    assert_eq!(subscription_row(&app, user_id).await["status"], "trialing");

    let started = app
        .post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert_eq!(
        started.body["url"],
        "https://checkout.stripe.test/c/pay/cs_1"
    );
    let request = fake.last_checkout();
    assert_eq!(request.user_id, user_id);
    assert_eq!(request.price_id, "price_setlyst_pro_month_3990_brl");
    assert_eq!(request.customer_id, "cus_1");
    assert!(request.coupon_id.is_none());
    assert!(
        request
            .success_url
            .starts_with("https://setlyst.test/en/dashboard/settings?checkout=success")
    );
    // The rest of the in-app trial carries over.
    let trial_ends: chrono::NaiveDateTime =
        sqlx::query_scalar("SELECT trial_ends_at FROM subscriptions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(request.trial_end, Some(trial_ends.and_utc().timestamp()));
    let customer: Option<String> =
        sqlx::query_scalar("SELECT stripe_customer_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(customer.as_deref(), Some("cus_1"));

    // A running promotion comes off the first charge, which still waits
    // for the trial to end; the customer is reused and the earlier page
    // is expired.
    sqlx::query(
        "INSERT INTO promotions (id, name, plan_code, discount_percent, starts_at, ends_at, active,
                                 created_at, updated_at)
         VALUES (gen_random_uuid(), 'Launch', 'pro', 20, NOW() - INTERVAL '1 day',
                 NOW() + INTERVAL '1 day', TRUE, NOW(), NOW())",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    app.post("/billing/checkout", &token, checkout_body("pro", "yearly"))
        .await;
    let request = fake.last_checkout();
    assert_eq!(request.price_id, "price_setlyst_pro_year_39900_brl");
    assert_eq!(request.coupon_id.as_deref(), Some("co_20"));
    assert_eq!(request.trial_end, Some(trial_ends.and_utc().timestamp()));
    assert_eq!(request.redemption_id, None);
    assert_eq!(fake.calls_to("customer:"), 1);
    assert_eq!(fake.calls_to("expire:cs_1"), 1);

    // A better redeemed discount code wins and is named on the checkout.
    let code_id: Uuid = sqlx::query_scalar(
        "INSERT INTO promo_codes (id, code, kind, discount_percent, created_at, updated_at)
         VALUES (gen_random_uuid(), 'MUSIC30', 'discount', 30, NOW(), NOW()) RETURNING id",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let redeemed = app
        .post("/billing/redeem", &token, json!({ "code": "music30" }))
        .await;
    assert_eq!(redeemed.status, StatusCode::OK, "{}", redeemed.body);
    app.post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    let request = fake.last_checkout();
    assert_eq!(request.coupon_id.as_deref(), Some("co_30"));
    let redemption: Uuid = sqlx::query_scalar(
        "SELECT id FROM promo_redemptions WHERE promo_code_id = $1 AND user_id = $2",
    )
    .bind(code_id)
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(request.redemption_id, Some(redemption));

    // Completing that checkout spends the code.
    fake.put(subscription("sub_anna", user_id, "pro", 30));
    let body = json!({
        "id": "evt_checkout", "object": "event", "type": "checkout.session.completed",
        "data": {"object": {
            "object": "checkout.session", "mode": "subscription", "subscription": "sub_anna",
            "client_reference_id": user_id.to_string(),
            "metadata": {"redemption_id": redemption.to_string()}
        }}
    });
    let signature = sign(
        body.to_string().as_bytes(),
        WEBHOOK_SECRET,
        Utc::now().timestamp(),
    );
    let (status, _) = deliver(&app, &body, Some(signature)).await;
    assert_eq!(status, StatusCode::OK);
    let applied: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT applied_at FROM promo_redemptions WHERE id = $1")
            .bind(redemption)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(applied.is_some());
    app.post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
}

#[tokio::test]
async fn the_webhook_verifies_signatures_and_mirrors_subscriptions_once() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "band.leader").await;
    fake.put(subscription("sub_1", user_id, "pro", 30));

    // Forged or unsigned deliveries change nothing.
    let body = json!({
        "id": "evt_forged", "object": "event", "type": "customer.subscription.created",
        "data": {"object": {"object": "subscription", "id": "sub_1"}}
    });
    let (unsigned, _) = deliver(&app, &body, None).await;
    assert_eq!(unsigned, StatusCode::BAD_REQUEST);
    let forged = sign(
        body.to_string().as_bytes(),
        "whsec_wrong",
        Utc::now().timestamp(),
    );
    let (forged, _) = deliver(&app, &body, Some(forged)).await;
    assert_eq!(forged, StatusCode::BAD_REQUEST);
    let stale = sign(
        body.to_string().as_bytes(),
        WEBHOOK_SECRET,
        Utc::now().timestamp() - 3600,
    );
    let (stale, _) = deliver(&app, &body, Some(stale)).await;
    assert_eq!(stale, StatusCode::BAD_REQUEST);
    assert_ne!(subscription_row(&app, user_id).await["source"], "payment");

    // A real one subscribes the account.
    assert_eq!(
        event(&app, "evt_1", "customer.subscription.created", "sub_1").await,
        StatusCode::OK
    );
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["status"], "active");
    assert_eq!(row["source"], "payment");
    assert_eq!(row["plan_code"], "pro");
    assert_eq!(row["billing_interval"], "monthly");
    assert_eq!(row["external_ref"], "sub_1");
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["plan"]["code"], "pro");
    assert_eq!(me.body["subscription"]["billing_interval"], "monthly");
    assert_eq!(
        event_kinds(&app, user_id).await,
        ["trial_started", "subscribed"]
    );
    app.wait_for_count(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'subscription_changed'",
        user_id,
        1,
    )
    .await;

    // Redeliveries and events with nothing new are no-ops.
    event(&app, "evt_1", "customer.subscription.created", "sub_1").await;
    event(&app, "evt_1b", "checkout.session.completed", "sub_1").await;
    assert_eq!(
        event_kinds(&app, user_id).await,
        ["trial_started", "subscribed"]
    );

    // A failed renewal keeps the plan while Stripe retries.
    fake.edit("sub_1", |s| s.status = "past_due".into());
    event(&app, "evt_2", "customer.subscription.updated", "sub_1").await;
    assert_eq!(subscription_row(&app, user_id).await["status"], "past_due");
    assert_eq!(
        app.get("/billing/me", &token).await.body["plan"]["code"],
        "pro"
    );

    fake.edit("sub_1", |s| {
        s.status = "active".into();
        s.cancel_at_period_end = true;
    });
    event(&app, "evt_3", "customer.subscription.updated", "sub_1").await;
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["cancel_at_period_end"], true);

    // Stripe ends it at the period end.
    fake.edit("sub_1", |s| {
        s.status = "canceled".into();
        s.ended_at = Some(Utc::now().timestamp());
    });
    event(&app, "evt_4", "customer.subscription.deleted", "sub_1").await;
    assert_eq!(subscription_row(&app, user_id).await["status"], "canceled");
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());
    assert_eq!(
        event_kinds(&app, user_id).await,
        // Recovery and the scheduled cancellation arrive in one update:
        // the recovery is what gets recorded.
        [
            "trial_started",
            "subscribed",
            "payment_failed",
            "payment_recovered",
            "canceled"
        ]
    );

    // A new purchase, then a late event about the old subscription: the
    // new one stays.
    fake.put(subscription("sub_2", user_id, "basic", 30));
    event(&app, "evt_5", "customer.subscription.created", "sub_2").await;
    event(&app, "evt_6", "customer.subscription.deleted", "sub_1").await;
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["external_ref"], "sub_2");
    assert_eq!(row["status"], "active");
    assert_eq!(row["plan_code"], "basic");

    // Events about subscriptions of nobody here are acknowledged.
    let mut stranger = subscription("sub_x", Uuid::now_v7(), "pro", 30);
    stranger.customer_id = "cus_unknown".into();
    fake.put(stranger);
    assert_eq!(
        event(&app, "evt_7", "customer.subscription.created", "sub_x").await,
        StatusCode::OK
    );
    // Stripe unreachable: answered with an error so Stripe retries.
    assert!(
        event(&app, "evt_8", "customer.subscription.updated", "sub_gone")
            .await
            .is_server_error()
    );
    let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stripe_events WHERE id = 'evt_8'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(recorded, 0, "a failed event is retried, not swallowed");
}

#[tokio::test]
async fn paid_plans_change_in_place_and_block_other_grants() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "session.player").await;

    let none = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("basic", "yearly"),
        )
        .await;
    assert_eq!(none.code(), "NO_PAID_SUBSCRIPTION");
    let no_portal = app.post("/billing/portal", &token, json!({})).await;
    assert_eq!(no_portal.code(), "NO_PAID_SUBSCRIPTION");

    fake.put(subscription("sub_1", user_id, "pro", 30));
    event(&app, "evt_1", "customer.subscription.created", "sub_1").await;

    // One paid subscription per account.
    let again = app
        .post(
            "/billing/checkout",
            &token,
            checkout_body("basic", "monthly"),
        )
        .await;
    assert_eq!(again.code(), "PAID_SUBSCRIPTION_ACTIVE");
    // Plan time from codes, credits or staff would replace what Stripe
    // keeps charging for.
    sqlx::query(
        "INSERT INTO promo_codes (id, code, kind, plan_code, duration_days, created_at, updated_at)
         VALUES (gen_random_uuid(), 'FREEPRO', 'plan_grant', 'pro', 30, NOW(), NOW())",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let grant = app
        .post("/billing/redeem", &token, json!({ "code": "FREEPRO" }))
        .await;
    assert_eq!(grant.code(), "PAID_SUBSCRIPTION_ACTIVE");
    let (_, admin) = app.user("billing.staff", Role::Admin).await;
    let staff_grant = app
        .put(
            &format!("/admin/users/{user_id}/subscription"),
            &admin,
            json!({ "plan_code": "basic", "days": 30 }),
        )
        .await;
    assert_eq!(staff_grant.code(), "PAID_SUBSCRIPTION_ACTIVE");

    // Switching plan and interval.
    let changed = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("basic", "yearly"),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.body["plan"]["code"], "basic");
    assert_eq!(changed.body["subscription"]["billing_interval"], "yearly");
    assert_eq!(
        fake.calls_to("change:sub_1:price_setlyst_basic_year_14900_brl"),
        1
    );
    assert_eq!(
        event_kinds(&app, user_id).await.last().unwrap(),
        "plan_changed"
    );
    let same = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("basic", "yearly"),
        )
        .await;
    assert_eq!(same.code(), "PLAN_ALREADY_ACTIVE");

    // A declined charge leaves the plan as it was.
    *fake.decline_changes.lock().unwrap() = true;
    let declined = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("pro", "monthly"),
        )
        .await;
    assert_eq!(declined.status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(declined.code(), "PAYMENT_DECLINED");
    assert_eq!(subscription_row(&app, user_id).await["plan_code"], "basic");
    *fake.decline_changes.lock().unwrap() = false;

    // Past due, or set to end: fix that in the portal first.
    fake.edit("sub_1", |s| s.cancel_at_period_end = true);
    let canceling = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("pro", "monthly"),
        )
        .await;
    assert_eq!(canceling.code(), "SUBSCRIPTION_CANCELING");
    fake.edit("sub_1", |s| {
        s.cancel_at_period_end = false;
        s.status = "past_due".into();
    });
    let past_due = app
        .post(
            "/billing/subscription/change",
            &token,
            checkout_body("pro", "monthly"),
        )
        .await;
    assert_eq!(past_due.code(), "SUBSCRIPTION_PAST_DUE");

    let portal = app.post("/billing/portal", &token, json!({})).await;
    assert_eq!(portal.status, StatusCode::OK, "{}", portal.body);
    assert_eq!(
        portal.body["url"],
        "https://billing.stripe.test/p/session/1"
    );
    assert_eq!(
        fake.calls_to(&format!(
            "portal:cus_{}:https://setlyst.test/en/dashboard/settings#subscription",
            user_id.simple()
        )),
        1
    );
}

#[tokio::test]
async fn revoking_or_deleting_cancels_the_charges() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (_, admin) = app.user("billing.boss", Role::Admin).await;

    let (revoked_id, _) = verified_user(&app, "refund.me").await;
    fake.put(subscription("sub_r", revoked_id, "pro", 30));
    event(&app, "evt_r", "customer.subscription.created", "sub_r").await;
    let revoke = app
        .delete(&format!("/admin/users/{revoked_id}/subscription"), &admin)
        .await;
    assert!(revoke.status.is_success(), "{}", revoke.body);
    assert_eq!(fake.calls_to("cancel:sub_r"), 1);
    assert_eq!(fake.get("sub_r").status, "canceled");

    let (leaving_id, leaving) = verified_user(&app, "moving.on").await;
    fake.put(subscription("sub_d", leaving_id, "pro", 30));
    event(&app, "evt_d", "customer.subscription.created", "sub_d").await;
    let deleted = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&leaving),
            Some(json!({ "confirmation": "moving.on", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert_eq!(fake.calls_to("cancel:sub_d"), 1);

    // Stripe's own "deleted" event then finds no account: acknowledged.
    assert_eq!(
        event(&app, "evt_d2", "customer.subscription.deleted", "sub_d").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn lapsed_paid_plans_are_checked_with_stripe_before_expiring() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "night.owl").await;
    fake.put(subscription("sub_1", user_id, "pro", 30));
    event(&app, "evt_1", "customer.subscription.created", "sub_1").await;
    let set_period_end = |days_ago: i64| {
        sqlx::query(
            "UPDATE subscriptions
             SET current_period_end = NOW() AT TIME ZONE 'utc' - make_interval(days => $2::INT)
             WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(days_ago as i32)
        .execute(&app.pool)
    };

    // The period ended a day ago here, but Stripe renewed it (the webhook
    // was lost): the renewal is mirrored.
    set_period_end(1).await.unwrap();
    run_subscription_maintenance(&app.state).await.unwrap();
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["status"], "active");
    assert_eq!(event_kinds(&app, user_id).await.last().unwrap(), "renewed");
    assert_eq!(
        app.get("/billing/me", &token).await.body["plan"]["code"],
        "pro"
    );

    // Past the grace period while Stripe can't be reached: kept, and
    // retried at the next run.
    set_period_end(3).await.unwrap();
    *fake.unreachable.lock().unwrap() = true;
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(subscription_row(&app, user_id).await["status"], "active");

    // Stripe no longer has it: it lapses.
    *fake.unreachable.lock().unwrap() = false;
    fake.subscriptions.lock().unwrap().remove("sub_1");
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(subscription_row(&app, user_id).await["status"], "expired");
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());

    // A renewal event arriving afterwards restores it.
    fake.put(subscription("sub_1", user_id, "pro", 30));
    event(&app, "evt_2", "customer.subscription.updated", "sub_1").await;
    assert_eq!(subscription_row(&app, user_id).await["status"], "active");
    assert_eq!(
        event_kinds(&app, user_id).await.last().unwrap(),
        "subscribed"
    );
}

#[tokio::test]
async fn a_failed_renewal_keeps_the_plan_for_14_days_then_ends() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "late.payer").await;
    fake.put(subscription("sub_1", user_id, "pro", 30));
    event(&app, "evt_1", "customer.subscription.created", "sub_1").await;

    fake.edit("sub_1", |s| s.status = "past_due".into());
    event(&app, "evt_2", "customer.subscription.updated", "sub_1").await;
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["plan"]["code"], "pro");
    assert!(me.body["past_due_since"].is_string(), "{}", me.body);
    // Stripe retrying again changes nothing.
    event(&app, "evt_3", "customer.subscription.updated", "sub_1").await;
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(fake.calls_to("cancel:sub_1"), 0);

    // Fifteen days of failed retries: no plan, and the subscription is
    // canceled at Stripe so it is never charged again.
    sqlx::query(
        "UPDATE subscriptions SET past_due_since = NOW() AT TIME ZONE 'utc' - INTERVAL '15 days'
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(fake.calls_to("cancel:sub_1"), 1);
    assert_eq!(subscription_row(&app, user_id).await["status"], "canceled");
    assert_eq!(event_kinds(&app, user_id).await.last().unwrap(), "canceled");
}

fn invoice(id: &str, user_id: Uuid, amount: i64, days_ago: i64) -> GatewayInvoice {
    GatewayInvoice {
        id: id.into(),
        customer_id: Some(format!("cus_{}", user_id.simple())),
        subscription_id: Some("sub_fin".into()),
        user_id: Some(user_id),
        payment_intent_id: Some(format!("pi_{id}")),
        plan_code: Some("pro".into()),
        interval: Some(BillingInterval::Monthly),
        amount_paid: amount,
        currency: "BRL".into(),
        paid_at: Some(Utc::now().timestamp() - days_ago * 86_400),
    }
}

async fn signed(app: &TestApp, body: Value) -> StatusCode {
    let signature = sign(
        body.to_string().as_bytes(),
        WEBHOOK_SECRET,
        Utc::now().timestamp(),
    );
    deliver(app, &body, Some(signature)).await.0
}

#[tokio::test]
async fn paid_invoices_and_refunds_feed_the_finance_report() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "paying.fan").await;
    let (_, admin) = app.user("finance.admin", Role::Admin).await;
    let (_, moderator) = app.user("finance.mod", Role::Moderator).await;

    // Staff other than admins don't see the money.
    assert_eq!(
        app.get("/admin/finance", &moderator).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/admin/finance", &token).await.status,
        StatusCode::FORBIDDEN
    );

    fake.put(subscription("sub_fin", user_id, "pro", 30));
    event(&app, "evt_sub", "customer.subscription.created", "sub_fin").await;
    fake.invoices
        .lock()
        .unwrap()
        .push(invoice("in_1", user_id, 2990, 0));

    // invoice.paid records the payment once, however often it arrives.
    let paid = json!({
        "id": "evt_paid", "object": "event", "type": "invoice.paid",
        "data": {"object": {"object": "invoice", "id": "in_1",
                 "parent": {"subscription_details": {"subscription": "sub_fin"}}}}
    });
    assert_eq!(signed(&app, paid.clone()).await, StatusCode::OK);
    assert_eq!(signed(&app, paid).await, StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payments")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    let report = app.get("/admin/finance", &admin).await;
    assert_eq!(report.status, StatusCode::OK, "{}", report.body);
    let body = &report.body;
    assert_eq!(body["paying_subscribers"], 1);
    assert_eq!(body["currency"], "BRL");
    assert!(body["mrr_cents"].as_i64().unwrap() > 0);
    assert_eq!(body["arr_cents"], body["mrr_cents"].as_i64().unwrap() * 12);
    assert_eq!(body["revenue"]["all_time_cents"], 2990);
    assert_eq!(body["revenue"]["last_12_months_cents"], 2990);
    assert_eq!(body["monthly"].as_array().unwrap().len(), 12);
    assert_eq!(body["new_subscribers_this_month"], 1);
    assert_eq!(body["recent_payments"][0]["username"], "paying.fan");
    assert_eq!(body["recent_payments"][0]["amount_cents"], 2990);

    // A partial refund, then the full one; a replay of the first changes
    // nothing.
    let refund = |amount: i64, id: &str| {
        json!({
            "id": id, "object": "event", "type": "charge.refunded",
            "data": {"object": {"object": "charge", "id": "ch_1",
                     "payment_intent": "pi_in_1", "amount_refunded": amount}}
        })
    };
    assert_eq!(signed(&app, refund(1000, "evt_r1")).await, StatusCode::OK);
    assert_eq!(signed(&app, refund(2990, "evt_r2")).await, StatusCode::OK);
    assert_eq!(signed(&app, refund(1000, "evt_r1")).await, StatusCode::OK);
    let report = app.get("/admin/finance", &admin).await;
    assert_eq!(report.body["revenue"]["all_time_cents"], 0);
    assert_eq!(report.body["recent_payments"][0]["refunded_cents"], 2990);

    // The sync brings in what the webhook never delivered, page by page.
    {
        let mut invoices = fake.invoices.lock().unwrap();
        invoices.insert(0, invoice("in_4", user_id, 2990, 1));
        invoices.insert(0, invoice("in_3", user_id, 2990, 2));
        invoices.insert(0, invoice("in_2", user_id, 2990, 3));
    }
    assert_eq!(
        app.post("/admin/finance/sync", &moderator, json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let sync = app.post("/admin/finance/sync", &admin, json!({})).await;
    assert_eq!(sync.status, StatusCode::OK, "{}", sync.body);
    assert_eq!(sync.body["done"], true);
    assert_eq!(sync.body["scanned"], 4);
    assert_eq!(sync.body["imported"], 3);
    let again = app.post("/admin/finance/sync", &admin, json!({})).await;
    assert_eq!(again.body["imported"], 0);
    let report = app.get("/admin/finance", &admin).await;
    assert_eq!(report.body["revenue"]["all_time_cents"], 3 * 2990);

    // Refunds the webhook never delivered come in with the sync too.
    fake.refunds.lock().unwrap().push(GatewayRefund {
        id: "re_1".into(),
        payment_intent_id: Some("pi_in_2".into()),
        amount: 990,
        status: "succeeded".into(),
        ..Default::default()
    });
    let sync = app.post("/admin/finance/sync", &admin, json!({})).await;
    assert_eq!(sync.body["refunds_applied"], 1);
    let report = app.get("/admin/finance", &admin).await;
    assert_eq!(report.body["revenue"]["all_time_cents"], 3 * 2990 - 990);

    // A refund that arrives before its invoice brings the invoice in.
    fake.invoices
        .lock()
        .unwrap()
        .insert(0, invoice("in_5", user_id, 2990, 0));
    let early_refund = json!({
        "id": "evt_r5", "object": "event", "type": "charge.refunded",
        "data": {"object": {"object": "charge", "id": "ch_5",
                 "payment_intent": "pi_in_5", "amount_refunded": 500}}
    });
    assert_eq!(signed(&app, early_refund).await, StatusCode::OK);
    let refunded: i64 =
        sqlx::query_scalar("SELECT refunded_cents FROM payments WHERE invoice_id = 'in_5'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(refunded, 500);

    // Deleting the account keeps the payments (anonymized) and removes
    // the customer at the provider.
    sqlx::query("UPDATE users SET stripe_customer_id = 'cus_gone' WHERE id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .unwrap();
    let deleted = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "password": STRONG_PASSWORD, "confirmation": "paying.fan" })),
        )
        .await;
    assert!(
        deleted.status.is_success(),
        "{}: {}",
        deleted.status,
        deleted.body
    );
    assert_eq!(fake.calls_to("delete_customer:cus_gone"), 1);
    let orphaned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payments WHERE user_id IS NULL")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(orphaned, 5);
}

/// A paid invoice of subscription `subscription_id`.
fn invoice_of(
    id: &str,
    subscription_id: &str,
    user_id: Uuid,
    amount: i64,
    days_ago: i64,
) -> GatewayInvoice {
    GatewayInvoice {
        subscription_id: Some(subscription_id.into()),
        ..invoice(id, user_id, amount, days_ago)
    }
}

/// Delivers a signed `invoice.paid` for `invoice_id`.
async fn invoice_paid(app: &TestApp, event_id: &str, invoice_id: &str, subscription_id: &str) {
    let body = json!({
        "id": event_id, "object": "event", "type": "invoice.paid",
        "data": {"object": {"object": "invoice", "id": invoice_id,
                 "parent": {"subscription_details": {"subscription": subscription_id}}}}
    });
    assert_eq!(signed(app, body).await, StatusCode::OK);
}

async fn outbox_count(app: &TestApp, user_id: Uuid, template: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM email_outbox WHERE user_id = $1 AND template = $2")
        .bind(user_id)
        .bind(template)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn duplicate_checkouts_are_refused_and_second_subscriptions_refunded() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "double.click").await;

    // A second checkout expires the page of the first.
    let first = app
        .post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let second = app
        .post("/billing/checkout", &token, checkout_body("pro", "yearly"))
        .await;
    assert_eq!(
        second.body["url"],
        "https://checkout.stripe.test/c/pay/cs_2"
    );
    assert_eq!(fake.calls_to("expire:cs_1"), 1);
    let expired: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT expired_at FROM checkout_sessions WHERE session_id = 'cs_1'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(expired.is_some());
    // Both checkouts were cards only and required the Subscription Terms.
    let request = fake.last_checkout();
    assert!(
        request
            .terms_message
            .as_deref()
            .unwrap()
            .contains("https://setlyst.test/en/legal/subscription")
    );
    assert!(request.idempotency_key.starts_with("setlyst-checkout-"));

    // Paid in one tab, the webhook not here yet: Stripe already has a live
    // subscription for the customer, so another checkout is refused.
    let mut paid = subscription("sub_a", user_id, "pro", 30);
    paid.customer_id = "cus_1".into();
    fake.put(paid);
    let refused = app
        .post(
            "/billing/checkout",
            &token,
            checkout_body("basic", "monthly"),
        )
        .await;
    assert_eq!(refused.code(), "PAID_SUBSCRIPTION_ACTIVE");
    assert_eq!(fake.calls_to("checkout"), 2);

    // The older tab gets paid anyway: the second subscription is canceled
    // and refunded instead of replacing the first.
    event(&app, "evt_a", "customer.subscription.created", "sub_a").await;
    let mut duplicate = subscription("sub_b", user_id, "pro", 365);
    duplicate.customer_id = "cus_1".into();
    duplicate.latest_invoice_id = Some("in_b".into());
    fake.put(duplicate);
    fake.invoices
        .lock()
        .unwrap()
        .push(invoice_of("in_b", "sub_b", user_id, 39900, 0));
    event(&app, "evt_b", "customer.subscription.created", "sub_b").await;
    assert_eq!(fake.calls_to("cancel:sub_b"), 1);
    assert_eq!(fake.calls_to("refund:pi_in_b:setlyst-duplicate-in_b"), 1);
    assert_eq!(fake.get("sub_b").status, "canceled");
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["external_ref"], "sub_a");
    assert_eq!(row["status"], "active");
    assert!(
        event_kinds(&app, user_id)
            .await
            .contains(&"duplicate_canceled".to_string())
    );
    // Stripe's "deleted" event for the duplicate changes nothing.
    event(&app, "evt_b2", "customer.subscription.deleted", "sub_b").await;
    assert_eq!(
        subscription_row(&app, user_id).await["external_ref"],
        "sub_a"
    );
}

#[tokio::test]
async fn withdrawal_within_7_days_refunds_and_cancels() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "changed.mind").await;
    fake.put(subscription("sub_w", user_id, "pro", 30));
    event(&app, "evt_w", "customer.subscription.created", "sub_w").await;
    assert_eq!(
        outbox_count(&app, user_id, "subscription_confirmed").await,
        1
    );
    assert!(app.get("/billing/me", &token).await.body["withdrawal_eligible_until"].is_null());

    fake.invoices
        .lock()
        .unwrap()
        .push(invoice_of("in_w1", "sub_w", user_id, 3990, 2));
    invoice_paid(&app, "evt_w1", "in_w1", "sub_w").await;
    let me = app.get("/billing/me", &token).await;
    assert!(
        me.body["withdrawal_eligible_until"].is_string(),
        "{}",
        me.body
    );
    assert!(me.body["past_due_since"].is_null());

    // Billing mails reach the inbox even with account e-mails off.
    sqlx::query(
        "INSERT INTO user_preferences (id, user_id, communication)
         VALUES (gen_random_uuid(), $1, '{\"categories\": {\"account\": {\"email\": false, \"in_app\": true}}}')
         ON CONFLICT (user_id) DO UPDATE SET communication = EXCLUDED.communication",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();

    let withdrawn = app.post("/billing/withdraw", &token, json!({})).await;
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    assert_eq!(withdrawn.body["refunded_cents"], 3990);
    assert_eq!(withdrawn.body["currency"], "brl");
    assert_eq!(fake.calls_to("refund:pi_in_w1:setlyst-withdraw-in_w1"), 1);
    assert_eq!(fake.calls_to("cancel:sub_w"), 1);
    assert_eq!(subscription_row(&app, user_id).await["status"], "canceled");
    assert_eq!(
        event_kinds(&app, user_id).await.last().unwrap(),
        "withdrawn"
    );
    assert_eq!(outbox_count(&app, user_id, "withdrawal_confirmed").await, 1);
    let me = app.get("/billing/me", &token).await;
    assert!(me.body["plan"].is_null());
    assert!(me.body["withdrawal_eligible_until"].is_null());
    let refunded: i64 =
        sqlx::query_scalar("SELECT refunded_cents FROM payments WHERE invoice_id = 'in_w1'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(refunded, 3990);
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'billing.subscription_withdrawn'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // Nothing left to withdraw from.
    let again = app.post("/billing/withdraw", &token, json!({})).await;
    assert_eq!(again.code(), "NO_PAID_SUBSCRIPTION");
    // Stripe's own events afterwards change nothing.
    let notices = outbox_count(&app, user_id, "subscription_changed").await;
    event(&app, "evt_w2", "customer.subscription.deleted", "sub_w").await;
    assert_eq!(
        outbox_count(&app, user_id, "subscription_changed").await,
        notices
    );
    assert_eq!(
        event_kinds(&app, user_id).await.last().unwrap(),
        "withdrawn"
    );
}

#[tokio::test]
async fn withdrawal_after_7_days_is_refused() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "too.late").await;
    fake.put(subscription("sub_l", user_id, "pro", 20));
    event(&app, "evt_l", "customer.subscription.created", "sub_l").await;
    fake.invoices
        .lock()
        .unwrap()
        .push(invoice_of("in_l", "sub_l", user_id, 3990, 10));

    let refused = app.post("/billing/withdraw", &token, json!({})).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "WITHDRAWAL_NOT_ELIGIBLE");
    assert!(refused.body["meta"]["eligible_until"].is_string());
    assert_eq!(fake.calls_to("refund:"), 0);
    assert_eq!(fake.calls_to("cancel:"), 0);
    assert_eq!(subscription_row(&app, user_id).await["status"], "active");

    let (_, free) = verified_user(&app, "never.paid").await;
    let none = app.post("/billing/withdraw", &free, json!({})).await;
    assert_eq!(none.code(), "NO_PAID_SUBSCRIPTION");
}

#[tokio::test]
async fn a_dispute_cancels_the_subscription_and_counts_against_revenue() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "charge.back").await;
    fake.put(subscription("sub_d", user_id, "pro", 30));
    event(&app, "evt_d", "customer.subscription.created", "sub_d").await;
    fake.invoices
        .lock()
        .unwrap()
        .push(invoice_of("in_d", "sub_d", user_id, 3990, 1));
    invoice_paid(&app, "evt_d1", "in_d", "sub_d").await;

    let dispute = |id: &str, kind: &str, status: &str| {
        json!({
            "id": id, "object": "event", "type": kind,
            "data": {"object": {"object": "dispute", "id": "dp_1", "charge": "ch_d",
                     "payment_intent": "pi_in_d", "amount": 3990, "status": status}}
        })
    };
    assert_eq!(
        signed(
            &app,
            dispute("evt_dp1", "charge.dispute.created", "needs_response")
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(fake.calls_to("cancel:sub_d"), 1);
    assert_eq!(subscription_row(&app, user_id).await["status"], "canceled");
    assert_eq!(event_kinds(&app, user_id).await.last().unwrap(), "disputed");
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());
    assert_eq!(outbox_count(&app, user_id, "payment_disputed").await, 1);
    let disputed = || async {
        sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT disputed_cents, dispute_status FROM payments WHERE invoice_id = 'in_d'",
        )
        .fetch_one(&app.pool)
        .await
        .unwrap()
    };
    assert_eq!(disputed().await, (3990, Some("needs_response".into())));

    // Won: the money came back.
    signed(&app, dispute("evt_dp2", "charge.dispute.closed", "won")).await;
    assert_eq!(disputed().await, (0, Some("won".into())));
    assert_eq!(outbox_count(&app, user_id, "payment_disputed").await, 1);
}

#[tokio::test]
async fn card_on_file_trials_are_reminded_once_before_the_first_charge() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, _) = verified_user(&app, "trial.card").await;
    let mut trialing = subscription("sub_t", user_id, "pro", 5);
    trialing.status = "trialing".into();
    trialing.trial_end = trialing.current_period_end;
    trialing.unit_amount = Some(3990);
    trialing.currency = Some("BRL".into());
    fake.put(trialing);
    event(&app, "evt_t", "customer.subscription.created", "sub_t").await;
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["provider_status"], "trialing");
    assert!(row["trial_ends_at"].is_string());
    // The confirmation names the first charge date.
    let (_, confirmation, _) = app
        .last_email("subscription_confirmed", None)
        .await
        .unwrap();
    assert!(confirmation["trial_ends_at"].is_string());

    event(
        &app,
        "evt_t1",
        "customer.subscription.trial_will_end",
        "sub_t",
    )
    .await;
    event(
        &app,
        "evt_t2",
        "customer.subscription.trial_will_end",
        "sub_t",
    )
    .await;
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(outbox_count(&app, user_id, "paid_trial_ending").await, 1);
    let (_, reminder, _) = app.last_email("paid_trial_ending", None).await.unwrap();
    assert_eq!(reminder["amount_cents"], 3990);
    assert_eq!(reminder["interval"], "monthly");

    // Trials don't count as recurring revenue until they pay.
    let (_, admin) = app.user("trial.admin", Role::Admin).await;
    let report = app.get("/admin/finance", &admin).await;
    assert_eq!(report.body["paying_subscribers"], 0);
    assert_eq!(report.body["paid_trialing"], 1);
}

#[tokio::test]
async fn yearly_renewals_are_reminded_once_per_period() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, _) = verified_user(&app, "yearly.fan").await;
    let mut yearly = subscription("sub_y", user_id, "pro", 20);
    yearly.interval = Some(BillingInterval::Yearly);
    fake.put(yearly);
    event(&app, "evt_y", "customer.subscription.created", "sub_y").await;

    let upcoming = |id: &str, subscription: &str| {
        json!({
            "id": id, "object": "event", "type": "invoice.upcoming",
            "data": {"object": {"object": "invoice", "amount_due": 39900, "currency": "brl",
                     "parent": {"subscription_details": {"subscription": subscription}}}}
        })
    };
    assert_eq!(
        signed(&app, upcoming("evt_u1", "sub_y")).await,
        StatusCode::OK
    );
    assert_eq!(
        signed(&app, upcoming("evt_u2", "sub_y")).await,
        StatusCode::OK
    );
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(outbox_count(&app, user_id, "renewal_reminder").await, 1);
    let (_, reminder, _) = app.last_email("renewal_reminder", None).await.unwrap();
    assert_eq!(reminder["amount_cents"], 39900);

    // Monthly plans get no such reminder.
    let (monthly_id, _) = verified_user(&app, "monthly.fan").await;
    fake.put(subscription("sub_m", monthly_id, "pro", 20));
    event(&app, "evt_m", "customer.subscription.created", "sub_m").await;
    signed(&app, upcoming("evt_u3", "sub_m")).await;
    assert_eq!(outbox_count(&app, monthly_id, "renewal_reminder").await, 0);
}

#[tokio::test]
async fn accepted_terms_are_stored_from_the_completed_checkout() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "terms.reader").await;
    let started = app
        .post("/billing/checkout", &token, checkout_body("pro", "monthly"))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    fake.put(subscription("sub_c", user_id, "pro", 30));
    let body = json!({
        "id": "evt_cs", "object": "event", "type": "checkout.session.completed",
        "data": {"object": {
            "object": "checkout.session", "id": "cs_1", "mode": "subscription",
            "subscription": "sub_c", "client_reference_id": user_id.to_string(),
            "consent": {"terms_of_service": "accepted"},
            "metadata": {"terms_version": "2026-09-24"}
        }}
    });
    assert_eq!(signed(&app, body).await, StatusCode::OK);
    let row = subscription_row(&app, user_id).await;
    assert_eq!(row["terms_version"], "2026-09-24");
    assert!(row["terms_accepted_at"].is_string());
    let completed: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT completed_at FROM checkout_sessions WHERE session_id = 'cs_1'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(completed.is_some());
}

#[tokio::test]
async fn events_from_the_other_stripe_mode_are_ignored() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, _) = verified_user(&app, "mode.mixup").await;
    fake.put(subscription("sub_live", user_id, "pro", 30));
    let body = json!({
        "id": "evt_live", "object": "event", "type": "customer.subscription.created",
        "livemode": true,
        "data": {"object": {"object": "subscription", "id": "sub_live"}}
    });
    assert_eq!(signed(&app, body).await, StatusCode::OK);
    assert_ne!(subscription_row(&app, user_id).await["source"], "payment");

    let body = json!({
        "id": "evt_test", "object": "event", "type": "customer.subscription.created",
        "livemode": false,
        "data": {"object": {"object": "subscription", "id": "sub_live"}}
    });
    assert_eq!(signed(&app, body).await, StatusCode::OK);
    assert_eq!(subscription_row(&app, user_id).await["source"], "payment");
}

#[tokio::test]
async fn customers_deleted_at_stripe_are_forgotten_and_emails_synced() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    let (user_id, _) = verified_user(&app, "gone.customer").await;
    sqlx::query("UPDATE users SET stripe_customer_id = 'cus_zap' WHERE id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .unwrap();

    setlyst_api::services::payments::sync_customer_email(&app.state, user_id)
        .await
        .unwrap();
    assert_eq!(
        fake.calls_to("update_customer:cus_zap:gone.customer@example.com"),
        1
    );

    let body = json!({
        "id": "evt_del", "object": "event", "type": "customer.deleted",
        "data": {"object": {"object": "customer", "id": "cus_zap"}}
    });
    assert_eq!(signed(&app, body).await, StatusCode::OK);
    let (customer, generation): (Option<String>, i32) = sqlx::query_as(
        "SELECT stripe_customer_id, stripe_customer_generation FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(customer, None);
    assert_eq!(generation, 1);
}

#[tokio::test]
async fn webhook_bodies_are_capped() {
    let mut app = app!();
    with_payments(&mut app);
    let body = json!({ "id": "evt_big", "object": "event", "padding": "x".repeat(300 * 1024) });
    let signature = sign(
        body.to_string().as_bytes(),
        WEBHOOK_SECRET,
        Utc::now().timestamp(),
    );
    let (status, _) = deliver(&app, &body, Some(signature)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn staff_refunds_cancel_now_and_refund_the_latest_charge() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "refund.me").await;
    fake.put(subscription("sub_r", user_id, "pro", 20));
    event(&app, "evt_r", "customer.subscription.created", "sub_r").await;
    {
        // Outside the withdrawal window: only the latest charge is refunded.
        let mut invoices = fake.invoices.lock().unwrap();
        invoices.push(invoice_of("in_r2", "sub_r", user_id, 3990, 10));
        invoices.push(invoice_of("in_r1", "sub_r", user_id, 3990, 40));
    }
    let (_, moderator) = app.user("refund.moderator", Role::Moderator).await;
    let (admin_id, admin) = app.user("refund.admin", Role::Admin).await;
    let path = format!("/admin/users/{user_id}/subscription/refund");
    let reason = json!({ "reason": "Service outage" });

    for caller in [&moderator, &token] {
        let refused = app.post(&path, caller, reason.clone()).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    }
    let blank = app.post(&path, &admin, json!({ "reason": "   " })).await;
    assert_eq!(blank.status, StatusCode::BAD_REQUEST, "{}", blank.body);
    let too_long = app
        .post(&path, &admin, json!({ "reason": "x".repeat(501) }))
        .await;
    assert_eq!(
        too_long.status,
        StatusCode::BAD_REQUEST,
        "{}",
        too_long.body
    );
    let unknown = app
        .post(
            &format!("/admin/users/{}/subscription/refund", Uuid::new_v4()),
            &admin,
            reason.clone(),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    assert_eq!(fake.calls_to("refund:"), 0);
    assert_eq!(fake.calls_to("cancel:"), 0);

    let refunded = app.post(&path, &admin, reason.clone()).await;
    assert_eq!(refunded.status, StatusCode::OK, "{}", refunded.body);
    assert_eq!(refunded.body["refunded_cents"], 3990);
    assert_eq!(refunded.body["currency"], "brl");
    assert_eq!(fake.calls_to("refund:pi_in_r2:setlyst-withdraw-in_r2"), 1);
    assert_eq!(fake.calls_to("refund:pi_in_r1"), 0);
    assert_eq!(fake.calls_to("cancel:sub_r"), 1);
    assert_eq!(subscription_row(&app, user_id).await["status"], "canceled");
    assert_eq!(event_kinds(&app, user_id).await.last().unwrap(), "refunded");
    assert_eq!(outbox_count(&app, user_id, "withdrawal_confirmed").await, 1);
    let (actor, target, logged_reason): (Option<Uuid>, Option<Uuid>, Option<String>) =
        sqlx::query_as(
            "SELECT actor_id, target_id, metadata->>'reason' FROM audit_logs
             WHERE action = 'billing.subscription_refunded'",
        )
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(actor, Some(admin_id));
    assert_eq!(target, Some(user_id));
    assert_eq!(logged_reason.as_deref(), Some("Service outage"));
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());

    let again = app.post(&path, &admin, reason).await;
    assert_eq!(again.code(), "NO_PAID_SUBSCRIPTION", "{}", again.body);
}

#[tokio::test]
async fn confirmed_email_changes_reach_the_payment_customer() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    let (user_id, token) = verified_user(&app, "moving.house").await;
    sqlx::query("UPDATE users SET stripe_customer_id = 'cus_move' WHERE id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .unwrap();

    let started = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "moved@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(started.status, StatusCode::ACCEPTED, "{}", started.body);
    assert_eq!(
        fake.calls_to("update_customer:"),
        0,
        "not before it's confirmed"
    );
    let code = app
        .last_code("email_change_code", "moved@example.com")
        .await;
    let confirmed = app
        .post(
            "/users/me/email/change/confirm",
            &token,
            json!({ "code": code }),
        )
        .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    assert_eq!(
        fake.calls_to("update_customer:cus_move:moved@example.com"),
        1
    );

    // A staff change of the address too.
    let (_, admin) = app.user("mail.admin", Role::Admin).await;
    let changed = app
        .patch(
            &format!("/users/{user_id}"),
            &admin,
            json!({ "email": "set.by.staff@example.com" }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(
        fake.calls_to("update_customer:cus_move:set.by.staff@example.com"),
        1
    );
}
