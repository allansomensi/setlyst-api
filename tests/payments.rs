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
        CheckoutRequest, GatewaySubscription, PaymentError, PaymentGateway, Payments, PriceSpec,
        webhook::sign,
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

    async fn create_coupon(&self, percent: i32, _name: &str) -> Result<String, PaymentError> {
        self.record(format!("coupon:{percent}"));
        Ok(format!("co_{percent}"))
    }

    async fn create_checkout(&self, request: &CheckoutRequest) -> Result<String, PaymentError> {
        self.record("checkout");
        self.checkouts.lock().unwrap().push(request.clone());
        Ok("https://checkout.stripe.test/c/pay/cs_1".into())
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
        has_pending_update: false,
    }
}

/// The app with the fake gateway plugged in.
fn with_payments(app: &mut TestApp) -> Arc<FakeGateway> {
    let fake = Arc::new(FakeGateway::default());
    app.state = app.state.clone().with_payments(Some(Payments {
        gateway: fake.clone(),
        webhook_secret: WEBHOOK_SECRET.into(),
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

    // A running promotion comes off the first charge (which then isn't
    // deferred), and the customer is reused.
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
    assert_eq!(request.trial_end, None);
    assert_eq!(request.redemption_id, None);
    assert_eq!(fake.calls_to("customer:"), 1);

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
async fn paid_plans_outlive_a_late_renewal_by_the_grace_period() {
    let mut app = app!();
    let fake = with_payments(&mut app);
    app.enforce_billing().await;
    let (user_id, token) = verified_user(&app, "night.owl").await;
    fake.put(subscription("sub_1", user_id, "pro", 30));
    event(&app, "evt_1", "customer.subscription.created", "sub_1").await;

    // The period ended a day ago and the renewal hasn't been confirmed.
    sqlx::query(
        "UPDATE subscriptions SET current_period_end = NOW() AT TIME ZONE 'utc' - INTERVAL '1 day'
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(subscription_row(&app, user_id).await["status"], "active");
    assert_eq!(
        app.get("/billing/me", &token).await.body["plan"]["code"],
        "pro"
    );

    // Past the grace period, it lapses.
    sqlx::query(
        "UPDATE subscriptions SET current_period_end = NOW() AT TIME ZONE 'utc' - INTERVAL '3 days'
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!(subscription_row(&app, user_id).await["status"], "expired");
    assert!(app.get("/billing/me", &token).await.body["plan"].is_null());

    // The renewal event arriving afterwards restores it.
    event(&app, "evt_2", "customer.subscription.updated", "sub_1").await;
    assert_eq!(subscription_row(&app, user_id).await["status"], "active");
    assert_eq!(
        event_kinds(&app, user_id).await.last().unwrap(),
        "subscribed"
    );
}
