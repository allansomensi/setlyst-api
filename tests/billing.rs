//! Plans, entitlements, trials, promo codes, credits and staff grants.

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::json;
use setlyst_api::{
    models::user::Role,
    services::{
        billing::run_subscription_maintenance,
        entitlements::{Feature, ensure_feature},
    },
};

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn limit(report: &serde_json::Value, resource: &str) -> serde_json::Value {
    report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["resource"] == resource)
        .unwrap()["limit"]
        .clone()
}

#[tokio::test]
async fn without_enforcement_everything_is_allowed_and_defaults_apply() {
    let app = app!();
    let (user_id, token) = app.registered_user("free.rider", "free@example.com").await;

    // Until the address is verified, the beta is very limited.
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.body["access"], "unverified");
    assert_eq!(me.body["email_verified"], false);
    assert_eq!(me.body["features"]["create_bands"], false);
    assert_eq!(me.body["features"]["pdf_export"], false);
    let refused = ensure_feature(&app.state, user_id, Feature::Tours)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "EMAIL_NOT_VERIFIED");
    let quotas = app.get("/users/me/quotas", &token).await;
    assert_eq!(limit(&quotas.body, "songs"), 20, "unverified tier");
    assert_eq!(limit(&quotas.body, "bands_owned"), 0);

    app.verify_email(&token, "free@example.com").await;
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["enforced"], false);
    assert_eq!(me.body["access"], "beta");
    assert_eq!(me.body["can_subscribe"], true);
    assert!(me.body["plan"].is_null());
    assert!(
        me.body["subscription"].is_null(),
        "no trial without enforcement"
    );
    assert!(
        me.body["features"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == true)
    );
    assert_eq!(
        me.body["referral"]["link_path"].as_str().unwrap().len(),
        "/register?ref=".len() + 10
    );
    assert_eq!(me.body["rewards"].as_array().unwrap().len(), 2);
    assert!(
        ensure_feature(&app.state, user_id, Feature::Tours)
            .await
            .is_ok()
    );

    let quotas = app.get("/users/me/quotas", &token).await;
    assert_eq!(limit(&quotas.body, "songs"), 1000, "platform default");

    // The public mode says so.
    let mode = app
        .request(Method::GET, "/public/billing", None, None)
        .await;
    assert_eq!(mode.status, StatusCode::OK, "{}", mode.body);
    assert_eq!(mode.body["beta"], true);
    assert_eq!(mode.body["beta_limits"]["songs"], 1000);
    assert_eq!(mode.body["free_limits"]["bands_owned"], 1);
    assert_eq!(mode.body["free_features"]["tours"], false);
    assert_eq!(mode.body["unverified_limits"]["songs"], 20);
}

#[tokio::test]
async fn enforcement_applies_plans_trials_and_limits() {
    let app = app!();
    let (_, admin) = app.user("billing.admin", Role::Admin).await;

    let settings = app.get("/admin/billing/settings", &admin).await;
    let mut settings = settings.body;
    settings["enforced"] = json!(true);
    settings["trial_plan"] = json!("nope");
    let unknown = app
        .put("/admin/billing/settings", &admin, settings.clone())
        .await;
    assert_eq!(unknown.code(), "PLAN_NOT_FOUND");
    settings["trial_plan"] = json!("pro");
    settings["trial_days"] = json!(14);
    let saved = app.put("/admin/billing/settings", &admin, settings).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    // New accounts start with a trial of the trial plan.
    let (_, token) = app.registered_user("trial.user", "trial@example.com").await;
    // The trial starts once the address is verified.
    app.verify_email(&token, "trial@example.com").await;
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["enforced"], true);
    assert_eq!(me.body["subscription"]["status"], "trialing");
    assert_eq!(me.body["plan"]["code"], "pro");
    assert_eq!(me.body["features"]["priority_support"], true);

    // Accounts without a plan get the free tier: one band, no tours, no
    // PDF or report exports.
    let (bare, bare_token) = app.user("bare.user", Role::User).await;
    let bare_me = app.get("/billing/me", &bare_token).await;
    assert!(bare_me.body["plan"].is_null());
    assert_eq!(bare_me.body["access"], "free");
    assert_eq!(bare_me.body["features"]["tours"], false);
    assert_eq!(bare_me.body["features"]["pdf_export"], false);
    assert_eq!(bare_me.body["features"]["analytics_export"], false);
    assert_eq!(bare_me.body["features"]["create_bands"], true);
    let free_quotas = app.get("/users/me/quotas", &bare_token).await;
    assert_eq!(limit(&free_quotas.body, "bands_owned"), 1);
    assert_eq!(limit(&free_quotas.body, "tours"), 0);
    let refused = ensure_feature(&app.state, bare, Feature::Tours)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "FEATURE_NOT_IN_PLAN");
    let body = {
        use axum::response::IntoResponse;
        let response = refused.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    assert_eq!(body["meta"]["feature"], "tours");
    assert_eq!(
        body["meta"]["plan"], "intermediate",
        "cheapest plan with tours"
    );

    // Granting a plan applies its limits (and per-user overrides on top).
    let granted = app
        .put(
            &format!("/admin/users/{bare}/subscription"),
            &admin,
            json!({ "plan_code": "basic", "days": 30, "note": "Support" }),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.body);
    assert_eq!(granted.body["subscription"]["plan_code"], "basic");
    assert_eq!(granted.body["subscription"]["source"], "admin");
    // Staff see who did it and the note; the account sees neither.
    assert_eq!(granted.body["events"][0]["data"]["note"], "Support");
    assert!(granted.body["events"][0]["actor_username"].is_string());
    let history = app.get("/billing/history", &bare_token).await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.body);
    let latest = &history.body[0];
    assert_eq!(latest["to_plan"], "basic", "{latest}");
    assert_eq!(latest["data"]["days"], 30, "{latest}");
    assert!(latest.get("actor_username").is_none(), "{latest}");
    assert!(latest["data"].get("note").is_none(), "{latest}");
    let quotas = app.get("/users/me/quotas", &bare_token).await;
    assert_eq!(limit(&quotas.body, "songs"), 250);
    assert_eq!(limit(&quotas.body, "tours"), 0);
    app.put(
        &format!("/users/{bare}/quotas"),
        &admin,
        json!({ "overrides": { "songs": 12 }, "unlimited": false }),
    )
    .await;
    let quotas = app.get("/users/me/quotas", &bare_token).await;
    assert_eq!(limit(&quotas.body, "songs"), 12);

    // Granting the same plan again extends it.
    let before = granted.body["subscription"]["current_period_end"]
        .as_str()
        .unwrap()
        .to_string();
    let extended = app
        .put(
            &format!("/admin/users/{bare}/subscription"),
            &admin,
            json!({ "plan_code": "basic", "days": 10 }),
        )
        .await;
    assert!(
        extended.body["subscription"]["current_period_end"]
            .as_str()
            .unwrap()
            > before.as_str()
    );
    assert_eq!(extended.body["events"][0]["kind"], "extended");

    // Revoking ends it now; the notification tells the user.
    let revoked = app
        .delete(&format!("/admin/users/{bare}/subscription"), &admin)
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    assert!(app.get("/billing/me", &bare_token).await.body["plan"].is_null());
    let history = app.get("/billing/history", &bare_token).await;
    assert_eq!(history.body[0]["kind"], "revoked");

    // Moderators may look but not grant.
    let (mod_id, moderator) = app.user("billing.mod", Role::Moderator).await;
    assert_eq!(
        app.get(&format!("/admin/users/{bare}/subscription"), &moderator)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        app.put(
            &format!("/admin/users/{bare}/subscription"),
            &moderator,
            json!({ "plan_code": "pro" })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let overview = app.get("/admin/billing/overview", &moderator).await;
    assert_eq!(overview.body["enforced"], true);

    // Staff have everything already and can't subscribe to anything.
    let mod_me = app.get("/billing/me", &moderator).await;
    assert_eq!(mod_me.body["access"], "staff");
    assert_eq!(mod_me.body["can_subscribe"], false);
    assert!(
        mod_me.body["features"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == true)
    );
    let mod_quotas = app.get("/users/me/quotas", &moderator).await;
    assert_eq!(mod_quotas.body["unlimited"], true);
    let staff_grant = app
        .put(
            &format!("/admin/users/{mod_id}/subscription"),
            &admin,
            json!({ "plan_code": "pro", "days": 30 }),
        )
        .await;
    assert_eq!(staff_grant.code(), "STAFF_CANNOT_SUBSCRIBE");
    let staff_checkout = app
        .post(
            "/billing/checkout",
            &moderator,
            json!({ "plan_code": "pro", "interval": "monthly" }),
        )
        .await;
    assert_eq!(staff_checkout.code(), "STAFF_CANNOT_SUBSCRIBE");

    // Grant trials to everyone without a subscription.
    // Staff and unverified accounts are left out (the latter get their
    // trial once they verify).
    let (later, later_token) = app.user("later.user", Role::User).await;
    let (unverified, _) = app.unverified_user("later.unverified", Role::User).await;
    let trials = app
        .post("/admin/billing/grant-trials", &admin, json!({ "days": 7 }))
        .await;
    assert!(
        trials.body["granted"].as_i64().unwrap() >= 1,
        "{}",
        trials.body
    );
    assert_eq!(
        app.get("/billing/me", &later_token).await.body["subscription"]["status"],
        "trialing"
    );
    for (id, expected) in [(later, true), (unverified, false), (mod_id, false)] {
        let has: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM subscriptions WHERE user_id = $1)")
                .bind(id)
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(has, expected, "{id}");
    }
}

#[tokio::test]
async fn maintenance_expires_periods_and_reminds_trials() {
    let app = app!();
    app.set_billing(json!({ "enforced": true })).await;
    let (user_id, token) = app
        .registered_user("expiring", "expiring@example.com")
        .await;
    let (reminded_id, reminded) = app
        .registered_user("reminded", "reminded@example.com")
        .await;
    // Trials start once the address is verified.
    app.verify_email(&token, "expiring@example.com").await;
    app.verify_email(&reminded, "reminded@example.com").await;

    sqlx::query("UPDATE subscriptions SET current_period_end = NOW() AT TIME ZONE 'utc' - INTERVAL '1 hour' WHERE user_id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE subscriptions SET trial_ends_at = NOW() AT TIME ZONE 'utc' + INTERVAL '2 days',
                current_period_end = NOW() AT TIME ZONE 'utc' + INTERVAL '2 days' WHERE user_id = $1",
    )
    .bind(reminded_id)
    .execute(&app.pool)
    .await
    .unwrap();

    let (expired, reminded) = run_subscription_maintenance(&app.state).await.unwrap();
    assert_eq!((expired, reminded), (1, 1));
    let me = app.get("/billing/me", &token).await;
    assert_eq!(me.body["subscription"]["status"], "expired");
    // Reminders go out once.
    assert_eq!(
        run_subscription_maintenance(&app.state).await.unwrap(),
        (0, 0)
    );
    let notified: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'trial_ending'",
    )
    .bind(reminded_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(notified, 1);
}

#[tokio::test]
async fn promo_codes_of_every_kind_and_their_errors() {
    let app = app!();
    let (_, admin) = app.user("promo.admin", Role::Admin).await;
    let (_, user) = app
        .registered_user("redeemer", "redeemer@example.com")
        .await;

    let create = |body: serde_json::Value| app.post("/admin/promo-codes", &admin, body);
    let mismatched = create(json!({ "kind": "credits", "duration_days": 3 })).await;
    assert_eq!(mismatched.code(), "VALIDATION_ERROR");

    let plan = create(
        json!({ "code": "PROPLAN30", "kind": "plan_grant", "plan_code": "pro", "duration_days": 30 }),
    )
    .await;
    assert_eq!(plan.status, StatusCode::CREATED, "{}", plan.body);
    create(json!({ "code": "TRIAL7DAYS", "kind": "trial_extension", "duration_days": 7 })).await;
    create(json!({ "code": "CREDITS50", "kind": "credits", "credits": 50 })).await;
    create(json!({ "code": "HALFPRICE", "kind": "discount", "discount_percent": 50 })).await;
    create(
        json!({ "code": "OLDCODE2020", "kind": "credits", "credits": 5,
                   "expires_at": "2020-01-01T00:00:00" }),
    )
    .await;
    let exhausted = create(
        json!({ "code": "ONCEONLY", "kind": "credits", "credits": 5, "max_redemptions": 1 }),
    )
    .await;
    create(json!({ "code": "NEWBIES25", "kind": "credits", "credits": 5, "new_users_only": true }))
        .await;
    let duplicate = create(json!({ "code": "proplan30", "kind": "credits", "credits": 1 })).await;
    assert_eq!(duplicate.code(), "ALREADY_EXISTS");
    // Codes worth money must not be guessable.
    let short = create(json!({ "code": "HALF", "kind": "discount", "discount_percent": 50 })).await;
    assert_eq!(short.code(), "VALIDATION_ERROR");

    let redeem = |code: &str| app.post("/billing/redeem", &user, json!({ "code": code }));

    let granted = redeem("proplan30").await;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.body);
    assert_eq!(granted.body["redemption"]["kind"], "plan_grant");
    assert_eq!(granted.body["subscription"]["plan_code"], "pro");
    assert_eq!(
        redeem("PROPLAN30").await.code(),
        "PROMO_CODE_ALREADY_REDEEMED"
    );

    // A trial extension doesn't apply to an account on a plan.
    assert_eq!(redeem("TRIAL7DAYS").await.code(), "PROMO_CODE_NOT_ELIGIBLE");

    let credits = redeem("credits50").await;
    assert_eq!(credits.body["credits"]["balance"], 50);
    let discount = redeem("HALFPRICE").await;
    assert_eq!(discount.body["redemption"]["discount_percent"], 50);

    assert_eq!(redeem("NOPE").await.code(), "PROMO_CODE_INVALID");
    assert_eq!(redeem("OLDCODE2020").await.code(), "PROMO_CODE_EXPIRED");
    // The account existed before the code was created.
    assert_eq!(redeem("NEWBIES25").await.code(), "PROMO_CODE_NOT_ELIGIBLE");

    let (_, other) = app.registered_user("first.come", "first@example.com").await;
    let won = app
        .post("/billing/redeem", &other, json!({ "code": "ONCEONLY" }))
        .await;
    assert_eq!(won.status, StatusCode::OK);
    assert_eq!(redeem("ONCEONLY").await.code(), "PROMO_CODE_EXHAUSTED");

    // Disabled codes stop working.
    let (_, fresh) = app.registered_user("late.comer", "late@example.com").await;
    let plan_id = plan.body["id"].as_str().unwrap();
    app.patch(
        &format!("/admin/promo-codes/{plan_id}"),
        &admin,
        json!({ "disabled": true }),
    )
    .await;
    assert_eq!(
        app.post("/billing/redeem", &fresh, json!({ "code": "PROPLAN30" }))
            .await
            .code(),
        "PROMO_CODE_INVALID"
    );
    // New users may use NEWBIES; trial extension starts a trial.
    assert_eq!(
        app.post("/billing/redeem", &fresh, json!({ "code": "NEWBIES25" }))
            .await
            .status,
        StatusCode::OK
    );
    let trial = app
        .post("/billing/redeem", &fresh, json!({ "code": "TRIAL7DAYS" }))
        .await;
    assert_eq!(
        trial.body["subscription"]["status"], "trialing",
        "{}",
        trial.body
    );

    let redemptions = app
        .get(
            &format!(
                "/admin/promo-codes/{}/redemptions",
                exhausted.body["id"].as_str().unwrap()
            ),
            &admin,
        )
        .await;
    assert_eq!(redemptions.body[0]["username"], "first.come");
    let listed = app.get("/admin/promo-codes?q=once", &admin).await;
    assert_eq!(listed.body["meta"]["total_items"], 1);
    assert_eq!(listed.body["data"][0]["redemptions_count"], 1);
}

#[tokio::test]
async fn the_last_slot_of_a_promo_code_goes_to_exactly_one_account() {
    let app = app!();
    let (_, admin) = app.user("race.admin", Role::Admin).await;
    app.post(
        "/admin/promo-codes",
        &admin,
        json!({ "code": "LASTONE01", "kind": "credits", "credits": 10, "max_redemptions": 1 }),
    )
    .await;
    let (_, a) = app.registered_user("racer.a", "a@example.com").await;
    let (_, b) = app.registered_user("racer.b", "b@example.com").await;

    let (ra, rb) = tokio::join!(
        app.post("/billing/redeem", &a, json!({ "code": "LASTONE01" })),
        app.post("/billing/redeem", &b, json!({ "code": "LASTONE01" })),
    );
    let mut statuses = [ra.status, rb.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{} / {}",
        ra.body,
        rb.body
    );
    let loser = if ra.status == StatusCode::OK { rb } else { ra };
    assert_eq!(loser.code(), "PROMO_CODE_EXHAUSTED");
}

#[tokio::test]
async fn credits_buy_rewards_and_staff_can_adjust_them() {
    let app = app!();
    let (_, admin) = app.user("credit.admin", Role::Admin).await;
    let (user_id, user) = app.registered_user("saver", "saver@example.com").await;

    let unknown = app
        .post(
            "/billing/credits/redeem",
            &user,
            json!({ "reward_id": "nope" }),
        )
        .await;
    assert_eq!(unknown.code(), "REWARD_NOT_FOUND");
    let poor = app
        .post(
            "/billing/credits/redeem",
            &user,
            json!({ "reward_id": "pro_30d" }),
        )
        .await;
    assert_eq!(poor.status, StatusCode::CONFLICT);
    assert_eq!(poor.code(), "INSUFFICIENT_CREDITS");
    assert_eq!(poor.body["meta"]["balance"], 0);
    assert_eq!(poor.body["meta"]["required"], 120);

    let below_zero = app
        .post(
            &format!("/admin/users/{user_id}/credits"),
            &admin,
            json!({ "amount": -5, "note": "x" }),
        )
        .await;
    assert_eq!(below_zero.code(), "INSUFFICIENT_CREDITS");
    let added = app
        .post(
            &format!("/admin/users/{user_id}/credits"),
            &admin,
            json!({ "amount": 200, "note": "Beta tester" }),
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.body);
    assert_eq!(added.body["credits_balance"], 200);

    let bought = app
        .post(
            "/billing/credits/redeem",
            &user,
            json!({ "reward_id": "pro_30d" }),
        )
        .await;
    assert_eq!(bought.status, StatusCode::OK, "{}", bought.body);
    assert_eq!(bought.body["credits"]["balance"], 80);
    assert_eq!(bought.body["subscription"]["plan_code"], "pro");
    assert_eq!(bought.body["subscription"]["source"], "credits");

    let ledger = app.get("/billing/credits", &user).await;
    assert_eq!(ledger.body["meta"]["total_items"], 2);
    assert_eq!(ledger.body["data"][0]["amount"], -120);
    assert_eq!(ledger.body["data"][0]["reason"], "reward_redemption");
}

#[tokio::test]
async fn public_plans_show_the_best_running_promotion_and_admins_edit_plans() {
    let app = app!();
    let (_, admin) = app.user("plan.admin", Role::Admin).await;

    let plans = app
        .request(axum::http::Method::GET, "/public/plans", None, None)
        .await;
    assert_eq!(plans.status, StatusCode::OK);
    let codes: Vec<&str> = plans
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["basic", "intermediate", "pro"]);
    assert!(plans.body[0]["promotion"].is_null());

    let now = chrono::Utc::now().naive_utc();
    let window = |days: i64| {
        (
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(days),
        )
    };
    let (start, end) = window(10);
    let everywhere = app
        .post(
            "/admin/promotions",
            &admin,
            json!({ "name": "Launch", "headline": { "pt-BR": "Lançamento", "en": "Launch" },
                    "discount_percent": 20, "starts_at": start, "ends_at": end }),
        )
        .await;
    assert_eq!(
        everywhere.status,
        StatusCode::CREATED,
        "{}",
        everywhere.body
    );
    app.post(
        "/admin/promotions",
        &admin,
        json!({ "name": "Pro week", "headline": { "en": "Pro week" }, "plan_code": "pro",
                "discount_percent": 40, "starts_at": start, "ends_at": end }),
    )
    .await;
    let bad_window = app
        .post(
            "/admin/promotions",
            &admin,
            json!({ "name": "Broken", "headline": {}, "discount_percent": 10, "starts_at": end, "ends_at": start }),
        )
        .await;
    assert_eq!(bad_window.code(), "VALIDATION_ERROR");

    let plans = app
        .request(axum::http::Method::GET, "/public/plans", None, None)
        .await;
    assert_eq!(plans.body[0]["promotion"]["discount_percent"], 20);
    assert_eq!(plans.body[2]["promotion"]["discount_percent"], 40);

    // Plans can be edited and hidden.
    let edited = app
        .put(
            "/admin/plans/basic",
            &admin,
            json!({ "price_monthly_cents": 990, "is_public": false, "features": { "tours": true } }),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    assert_eq!(edited.body["features"]["tours"], true);
    let bogus = app
        .put(
            "/admin/plans/basic",
            &admin,
            json!({ "features": { "teleport": true } }),
        )
        .await;
    assert_eq!(bogus.code(), "VALIDATION_ERROR");
    // Stripe can't charge less than R$ 0,50.
    let too_cheap = app
        .put(
            "/admin/plans/basic",
            &admin,
            json!({ "price_monthly_cents": 49 }),
        )
        .await;
    assert_eq!(too_cheap.code(), "VALIDATION_ERROR");
    let nameless = app.put("/admin/plans/studio", &admin, json!({})).await;
    assert_eq!(nameless.code(), "VALIDATION_ERROR");
    let plans = app
        .request(axum::http::Method::GET, "/public/plans", None, None)
        .await;
    assert_eq!(plans.body.as_array().unwrap().len(), 2);
    assert_eq!(
        app.get("/admin/plans", &admin)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Any staff member can read plans (hidden ones too); only admins write.
    let (_, moderator) = app.user("plan.mod", Role::Moderator).await;
    let (_, user) = app.user("plan.user", Role::User).await;
    let listed = app.get("/admin/plans", &moderator).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body.as_array().unwrap().len(), 3);
    let basic = app.get("/admin/plans/basic", &moderator).await;
    assert_eq!(basic.status, StatusCode::OK, "{}", basic.body);
    assert_eq!(basic.body["code"], "basic");
    assert_eq!(basic.body["is_public"], false);
    assert_eq!(
        app.get("/admin/plans/studio", &moderator).await.code(),
        "PLAN_NOT_FOUND"
    );
    assert_eq!(
        app.get("/admin/plans", &user).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/admin/plans/basic", &user).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.put(
            "/admin/plans/basic",
            &moderator,
            json!({ "price_monthly_cents": 1 })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn plans_stay_enforced_while_paid_subscriptions_are_live() {
    let app = app!();
    let (_, admin) = app.user("enforce.admin", Role::Admin).await;
    let (user_id, _) = app.user("still.paying", Role::User).await;
    app.enforce_billing().await;
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_code, status, source, started_at, current_period_end,
                                    external_ref, created_at, updated_at)
         VALUES ($1, 'pro', 'active', 'payment', NOW(), NOW() + INTERVAL '20 days', 'sub_1', NOW(), NOW())",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let mut settings = app.get("/admin/billing/settings", &admin).await.body;
    settings["enforced"] = json!(false);
    let blocked = app
        .put("/admin/billing/settings", &admin, settings.clone())
        .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);
    assert_eq!(blocked.code(), "BILLING_HAS_PAID_SUBSCRIPTIONS");
    assert_eq!(blocked.body["meta"]["live"], 1);
    let forced = app
        .put("/admin/billing/settings?force=true", &admin, settings)
        .await;
    assert_eq!(forced.status, StatusCode::OK, "{}", forced.body);
}

#[tokio::test]
async fn the_registration_trial_starts_at_most_once() {
    let app = app!();
    app.set_billing(json!({ "enforced": true })).await;
    let (user_id, token) = app
        .registered_user("one.trial", "one.trial@example.com")
        .await;
    let trials = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM subscription_events WHERE user_id = $1 AND kind = 'trial_started'",
        )
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
    };
    // Nothing until the address is verified: the trial is what makes a
    // throwaway sign-up worth something.
    assert_eq!(trials().await, 0);
    let verified = app.verify_email(&token, "one.trial@example.com").await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    assert_eq!(trials().await, 1);
    // Called again, even after the trial ended: nothing new.
    sqlx::query("UPDATE subscriptions SET status = 'expired' WHERE user_id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .unwrap();
    setlyst_api::services::billing::start_registration_trial(&app.state, user_id)
        .await
        .unwrap();
    assert_eq!(trials().await, 1);
}

/// Enables the public link of `/{kind}/{id}` and returns its token.
async fn share(app: &TestApp, token: &str, kind: &str, id: &str) -> String {
    let shared = app
        .post(&format!("/{kind}/{id}/share"), token, json!({}))
        .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.body);
    shared.body["share_token"].as_str().unwrap().to_string()
}

async fn public_status(app: &TestApp, path: &str) -> StatusCode {
    app.request(axum::http::Method::GET, path, None, None)
        .await
        .status
}

#[tokio::test]
async fn public_links_stop_working_when_the_owner_loses_public_sharing() {
    let app = app!();
    let (sharer_id, sharer) = app.user("lapsed.sharer", Role::User).await;
    // Admins keep every feature, so their band stays shareable.
    let (_, staff_leader) = app.user("staff.leader", Role::Admin).await;
    let (_, free_leader) = app.user("free.leader", Role::User).await;

    let setlist = app.setlist(&sharer, "Personal", None).await;
    let setlist_token = share(&app, &sharer, "setlists", &setlist).await;
    let gig = app
        .post(
            "/gigs",
            &sharer,
            json!({ "venue": "Bar", "scheduled_at": "2030-03-01T22:00:00" }),
        )
        .await;
    assert_eq!(gig.status, StatusCode::CREATED, "{}", gig.body);
    let gig_token = share(&app, &sharer, "gigs", gig.body["id"].as_str().unwrap()).await;

    // Band setlists created by the same (soon free) member.
    let covered_band = app.band(&staff_leader, "Covered").await;
    app.join_band(&staff_leader, &sharer, &covered_band, Some("admin"))
        .await;
    let covered = app
        .setlist(&sharer, "Covered set", Some(&covered_band))
        .await;
    let covered_token = share(&app, &sharer, "setlists", &covered).await;
    let bare_band = app.band(&free_leader, "Bare").await;
    app.join_band(&free_leader, &sharer, &bare_band, Some("admin"))
        .await;
    let bare = app.setlist(&sharer, "Bare set", Some(&bare_band)).await;
    let bare_token = share(&app, &sharer, "setlists", &bare).await;

    let setlist_path = format!("/public/setlists/{setlist_token}");
    let pdf_path = format!("/public/setlists/{setlist_token}/export/pdf");
    let gig_path = format!("/public/gigs/{gig_token}");
    let covered_path = format!("/public/setlists/{covered_token}");
    let bare_path = format!("/public/setlists/{bare_token}");
    for path in [&setlist_path, &gig_path, &covered_path, &bare_path] {
        assert_eq!(public_status(&app, path).await, StatusCode::OK, "{path}");
    }

    // Subscriptions enforced and nobody but the admin on a plan: the
    // links are gone, except the band whose owner still has the feature.
    app.enforce_billing().await;
    for path in [&setlist_path, &pdf_path, &gig_path, &bare_path] {
        assert_eq!(
            public_status(&app, path).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(public_status(&app, &covered_path).await, StatusCode::OK);

    // A plan with public sharing brings the (still stored) link back.
    let (_, admin) = app.user("billing.admin", Role::Admin).await;
    let granted = app
        .put(
            &format!("/admin/users/{sharer_id}/subscription"),
            &admin,
            json!({ "plan_code": "basic", "days": 30, "note": "Support" }),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.body);
    assert_eq!(public_status(&app, &setlist_path).await, StatusCode::OK);
    assert_eq!(public_status(&app, &bare_path).await, StatusCode::OK);
}
