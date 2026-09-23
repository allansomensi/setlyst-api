//! Accounts and authentication (v0.12): registration, sign-in by e-mail,
//! lockout, two-factor authentication, password recovery, e-mail
//! verification and change, Google sign-in, communication preferences,
//! unsubscribe links, consent and self-service deletion.

mod common;

use axum::http::{Method, StatusCode};
use common::{STRONG_PASSWORD, TestApp};
use serde_json::json;
use setlyst_api::{
    email::unsubscribe,
    models::{communication::Category, notification::Notification, user::Role},
    services::notifier::notify,
    utils::totp,
};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn now_unix() -> u64 {
    chrono::Utc::now().timestamp() as u64
}

#[tokio::test]
async fn registration_requires_an_email_and_consent() {
    let app = app!();

    let no_email = app
        .post_public(
            "/auth/register",
            json!({ "username": "ana.maria", "password": STRONG_PASSWORD, "accept_terms": true }),
        )
        .await;
    assert_eq!(no_email.status, StatusCode::BAD_REQUEST);
    assert_eq!(no_email.code(), "EMAIL_REQUIRED");

    let no_terms = app
        .post_public(
            "/auth/register",
            json!({ "username": "ana.maria", "email": "ana@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(no_terms.code(), "TERMS_NOT_ACCEPTED");

    let bad_email = app.register("ana.maria", "not-an-email", json!({})).await;
    assert_eq!(bad_email.code(), "VALIDATION_ERROR");

    let created = app
        .register(
            "ana.maria",
            "Ana@Example.com",
            json!({ "locale": "pt-BR", "marketing_opt_in": true }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["email_verified"], false);
    assert_eq!(created.body["terms_version"], "2026-09-23");
    assert_eq!(created.body["password_set"], true);
    assert!(created.body["referral_code"].as_str().unwrap().len() == 10);
    assert!(created.body.get("password_hash").is_none());

    // A verification code went out, in the chosen language.
    let (to, payload, _) = app
        .last_email("email_verification_code", Some("ana@example.com"))
        .await
        .expect("verification e-mail");
    assert_eq!(to, "Ana@Example.com");
    assert_eq!(payload["code"].as_str().unwrap().len(), 6);

    // E-mail uniqueness ignores case.
    let taken = app
        .register("other.user", "ANA@example.COM", json!({}))
        .await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.code(), "EMAIL_TAKEN");

    // Marketing opt-in was stored.
    let token = app.login("ana.maria", STRONG_PASSWORD).await;
    let prefs = app.get("/users/me/communication", &token).await;
    assert_eq!(prefs.body["categories"]["marketing"]["email"], true);
    assert_eq!(prefs.body["categories"]["security"]["locked"], true);
}

#[tokio::test]
async fn referrals_are_rewarded_on_email_verification_but_not_from_the_same_ip() {
    let app = app!();
    let (_, referrer_token) = app
        .registered_user("referrer", "referrer@example.com")
        .await;
    let code = app.get("/users/me", &referrer_token).await.body["referral_code"]
        .as_str()
        .unwrap()
        .to_string();

    // A referred account from another network.
    let referred = app
        .register(
            "friend",
            "friend@example.com",
            json!({ "referral_code": code.to_lowercase() }),
        )
        .await;
    assert_eq!(referred.status, StatusCode::CREATED, "{}", referred.body);
    let friend_token = app.login("friend", STRONG_PASSWORD).await;

    let pending = app.get("/billing/referrals", &referrer_token).await;
    assert_eq!(pending.body["data"][0]["status"], "pending");

    let wrong = app
        .post(
            "/users/me/email/verify",
            &friend_token,
            json!({ "code": "000000" }),
        )
        .await;
    assert_eq!(wrong.code(), "INVALID_CODE");
    assert_eq!(wrong.body["meta"]["attempts_left"], 4);

    let verified = app.verify_email(&friend_token, "friend@example.com").await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    assert_eq!(verified.body["email_verified"], true);

    let referrer_billing = app.get("/billing/me", &referrer_token).await;
    assert_eq!(referrer_billing.body["credits"]["balance"], 50);
    assert_eq!(referrer_billing.body["referral"]["rewarded_count"], 1);
    let friend_billing = app.get("/billing/me", &friend_token).await;
    assert_eq!(friend_billing.body["credits"]["balance"], 20);

    // Verifying twice is refused.
    let again = app
        .post("/users/me/email/verification", &friend_token, json!({}))
        .await;
    assert_eq!(again.code(), "EMAIL_ALREADY_VERIFIED");

    // Same registration IP as the referrer: rejected (self-referral).
    let same_ip = [("x-forwarded-for", "203.0.113.77")];
    let body = |username: &str, email: &str, referral: Option<&str>| {
        json!({ "username": username, "email": email, "password": STRONG_PASSWORD,
                "accept_terms": true, "referral_code": referral })
    };
    let first = app
        .request_with_headers(
            Method::POST,
            "/auth/register",
            None,
            Some(body("self.one", "one@example.com", None)),
            &same_ip,
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED);
    let first_token = app.login("self.one", STRONG_PASSWORD).await;
    let first_code = app.get("/users/me", &first_token).await.body["referral_code"]
        .as_str()
        .unwrap()
        .to_string();
    let second = app
        .request_with_headers(
            Method::POST,
            "/auth/register",
            None,
            Some(body("self.two", "two@example.com", Some(&first_code))),
            &same_ip,
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED);
    let second_token = app.login("self.two", STRONG_PASSWORD).await;
    app.verify_email(&second_token, "two@example.com").await;
    let referrals = app.get("/billing/referrals", &first_token).await;
    assert_eq!(referrals.body["data"][0]["status"], "rejected");
    assert_eq!(
        app.get("/billing/me", &first_token).await.body["credits"]["balance"],
        0
    );
}

#[tokio::test]
async fn sign_in_by_email_and_lockout_after_repeated_failures() {
    let app = app!();
    app.registered_user("locky", "locky@example.com").await;

    let by_email = app
        .login_response("LOCKY@example.com", STRONG_PASSWORD)
        .await;
    assert_eq!(by_email.status, StatusCode::OK, "{}", by_email.body);
    assert_eq!(by_email.body["two_factor_required"], false);
    assert_eq!(by_email.body["terms_accepted"], true);
    assert_eq!(by_email.body["email_verified"], false);

    for attempt in 1..=4 {
        let wrong = app.login_response("locky", "Wr0ng!Password").await;
        assert_eq!(wrong.code(), "INVALID_CREDENTIALS", "attempt {attempt}");
    }
    let fifth = app.login_response("locky", "Wr0ng!Password").await;
    assert_eq!(fifth.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(fifth.code(), "ACCOUNT_LOCKED");
    assert!(fifth.body["meta"]["until"].is_string());

    // Even the right password is refused while locked.
    let locked = app.login_response("locky", STRONG_PASSWORD).await;
    assert_eq!(locked.code(), "ACCOUNT_LOCKED");

    // Unknown accounts still look like wrong passwords, and are audited
    // in the background.
    let unknown = app.login_response("nobody.here", STRONG_PASSWORD).await;
    assert_eq!(unknown.code(), "INVALID_CREDENTIALS");
    let audited = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.login_locked' AND target_id = $1",
            app.state
                .user_repo
                .find_by_identifier("locky")
                .await
                .unwrap()
                .unwrap()
                .id,
            1,
        )
        .await;
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn two_factor_authentication_full_flow() {
    let app = app!();
    let (user_id, token) = app.registered_user("secure", "secure@example.com").await;

    let no_password = app.post("/users/me/2fa/setup", &token, json!({})).await;
    assert_eq!(no_password.code(), "WRONG_PASSWORD");

    let setup = app
        .post(
            "/users/me/2fa/setup",
            &token,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);
    let secret_b32 = setup.body["secret"].as_str().unwrap().to_string();
    assert!(
        setup.body["otpauth_url"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/Setlyst:secure?secret=")
    );
    let secret = totp::base32_decode(&secret_b32).unwrap();

    let wrong = app
        .post("/users/me/2fa/enable", &token, json!({ "code": "000000" }))
        .await;
    assert_eq!(wrong.code(), "INVALID_TWO_FACTOR_CODE");

    let enabled = app
        .post(
            "/users/me/2fa/enable",
            &token,
            json!({ "code": totp::code_at(&secret, now_unix()) }),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
    let recovery: Vec<String> = enabled.body["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 10);
    assert!(
        app.last_email("two_factor_enabled", Some("secure@example.com"))
            .await
            .is_some()
    );

    let security = app.get("/users/me/security", &token).await;
    assert_eq!(security.body["two_factor_enabled"], true);
    assert_eq!(security.body["recovery_codes_remaining"], 10);

    // Sign-in now stops at the second factor.
    let first_step = app.login_response("secure", STRONG_PASSWORD).await;
    assert_eq!(first_step.status, StatusCode::OK);
    assert_eq!(first_step.body["two_factor_required"], true);
    assert!(first_step.body.get("token").is_none());
    let challenge = first_step.body["challenge_token"]
        .as_str()
        .unwrap()
        .to_string();

    let bad = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge, "code": "000000" }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(bad.code(), "INVALID_TWO_FACTOR_CODE");
    assert_eq!(bad.body["meta"]["attempts_left"], 4);

    // The code used to enable is spent; the next step's code is accepted.
    let next_code = totp::code_at(&secret, now_unix() + 30);
    let ok = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge, "code": next_code }),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert!(ok.body["token"].is_string());

    // A challenge works once, and a TOTP code works once.
    let reused = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge, "code": next_code }),
        )
        .await;
    assert_eq!(reused.code(), "INVALID_TWO_FACTOR_CODE");
    let second = app.login_response("secure", STRONG_PASSWORD).await;
    let challenge2 = second.body["challenge_token"].as_str().unwrap().to_string();
    let replay = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge2, "code": next_code }),
        )
        .await;
    assert_eq!(replay.code(), "INVALID_TWO_FACTOR_CODE");

    // Recovery codes: single use, any spacing or case.
    let with_recovery = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge2, "recovery_code": recovery[0].to_lowercase().replace('-', " ") }),
        )
        .await;
    assert_eq!(
        with_recovery.status,
        StatusCode::OK,
        "{}",
        with_recovery.body
    );
    let third = app.login_response("secure", STRONG_PASSWORD).await;
    let challenge3 = third.body["challenge_token"].as_str().unwrap().to_string();
    let recovery_again = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge3, "recovery_code": recovery[0] }),
        )
        .await;
    assert_eq!(recovery_again.code(), "INVALID_TWO_FACTOR_CODE");

    let fresh = with_recovery.body["token"].as_str().unwrap().to_string();
    assert_eq!(
        app.get("/users/me/security", &fresh).await.body["recovery_codes_remaining"],
        9
    );

    // Disabling needs the password and a code.
    let missing_code = app
        .post(
            "/users/me/2fa/disable",
            &fresh,
            json!({ "password": STRONG_PASSWORD, "code": "000000" }),
        )
        .await;
    assert_eq!(missing_code.code(), "INVALID_TWO_FACTOR_CODE");
    let disabled = app
        .post(
            "/users/me/2fa/disable",
            &fresh,
            json!({ "password": STRONG_PASSWORD, "code": recovery[1] }),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::NO_CONTENT, "{}", disabled.body);
    let plain = app.login_response("secure", STRONG_PASSWORD).await;
    assert!(plain.body["token"].is_string());
    let audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE target_id = $1
         AND action IN ('user.two_factor_enabled', 'user.two_factor_disabled')",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audit, 2);
}

#[tokio::test]
async fn password_recovery_does_not_enumerate_and_revokes_sessions() {
    let app = app!();
    let (user_id, old_token) = app
        .registered_user("forgetful", "forgetful@example.com")
        .await;

    let unknown = app
        .post_public(
            "/auth/password/forgot",
            json!({ "identifier": "ghost@example.com" }),
        )
        .await;
    let known = app
        .post_public(
            "/auth/password/forgot",
            json!({ "identifier": "forgetful" }),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::ACCEPTED);
    assert_eq!(known.status, StatusCode::ACCEPTED);
    assert_eq!(unknown.body, known.body);
    assert_eq!(known.body, json!({}));
    let code = app
        .last_code("password_reset_code", "forgetful@example.com")
        .await;

    let weak = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "forgetful", "code": code, "new_password": "weak" }),
        )
        .await;
    assert_eq!(weak.code(), "WEAK_PASSWORD");

    // Five wrong guesses burn the code.
    for left in (0..5).rev() {
        let wrong = app
            .post_public(
                "/auth/password/reset",
                json!({ "identifier": "forgetful", "code": "999999", "new_password": "N3w!Password" }),
            )
            .await;
        assert_eq!(wrong.code(), "INVALID_CODE");
        assert_eq!(wrong.body["meta"]["attempts_left"], left);
    }
    let burned = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "forgetful", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(burned.code(), "INVALID_CODE");

    // A new code works (the resend interval is skipped for the test).
    app.age_codes(user_id).await;
    app.post_public(
        "/auth/password/forgot",
        json!({ "identifier": "forgetful@example.com" }),
    )
    .await;
    let code = app
        .last_code("password_reset_code", "forgetful@example.com")
        .await;
    let reset = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "forgetful@example.com", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    assert_eq!(reset.body["reauth_required"], true);

    assert_eq!(
        app.get("/users/me", &old_token).await.code(),
        "SESSION_REVOKED"
    );
    let new_token = app.login("forgetful", "N3w!Password").await;
    let me = app.get("/users/me", &new_token).await;
    assert_eq!(
        me.body["email_verified"], true,
        "possession of the inbox was proven"
    );
    assert!(
        app.last_email("password_changed", Some("forgetful@example.com"))
            .await
            .is_some()
    );

    // The code is single use.
    let reused = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "forgetful", "code": code, "new_password": "An0ther!Password" }),
        )
        .await;
    assert_eq!(reused.code(), "INVALID_CODE");
}

#[tokio::test]
async fn email_codes_are_rate_limited_and_the_email_changes_through_a_code() {
    let app = app!();
    let (_, token) = app.registered_user("mover", "old@example.com").await;

    // Registration already sent a code: the next one must wait a minute.
    let too_soon = app
        .post("/users/me/email/verification", &token, json!({}))
        .await;
    assert_eq!(too_soon.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(too_soon.code(), "TOO_MANY_ATTEMPTS");
    assert!(
        too_soon.body["meta"]["retry_after_seconds"]
            .as_i64()
            .unwrap()
            > 0
    );

    // Direct e-mail edits are refused.
    let direct = app
        .patch("/users/me", &token, json!({ "email": "new@example.com" }))
        .await;
    assert_eq!(direct.status, StatusCode::BAD_REQUEST);
    assert_eq!(direct.code(), "BAD_REQUEST");

    let no_password = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "new@example.com" }),
        )
        .await;
    assert_eq!(no_password.code(), "WRONG_PASSWORD");

    app.registered_user("someone", "taken@example.com").await;
    let taken = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "TAKEN@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(taken.code(), "EMAIL_TAKEN");

    let started = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "new@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(started.status, StatusCode::ACCEPTED, "{}", started.body);
    let code = app.last_code("email_change_code", "new@example.com").await;
    let confirmed = app
        .post(
            "/users/me/email/change/confirm",
            &token,
            json!({ "code": code }),
        )
        .await;
    assert_eq!(confirmed.status, StatusCode::OK, "{}", confirmed.body);
    assert_eq!(confirmed.body["email"], "new@example.com");
    assert_eq!(confirmed.body["email_verified"], true);

    // The previous address got a (masked) notice.
    let (to, payload, _) = app
        .last_email("email_changed_notice", None)
        .await
        .expect("notice");
    assert_eq!(to, "old@example.com");
    assert_eq!(payload["new_email_masked"], "n***@example.com");

    // Signing in works with the new address.
    assert_eq!(
        app.login_response("new@example.com", STRONG_PASSWORD)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn google_sign_in_creates_links_and_challenges_accounts() {
    let app = app!();

    // A new account needs consent first.
    let consent = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-1:new.person@gmail.com:New Person" }),
        )
        .await;
    assert_eq!(consent.status, StatusCode::BAD_REQUEST);
    assert_eq!(consent.code(), "TERMS_NOT_ACCEPTED");
    assert_eq!(consent.body["meta"]["signup"], true);
    assert_eq!(consent.body["meta"]["email"], "new.person@gmail.com");
    assert_eq!(consent.body["meta"]["name"], "New Person");

    let created = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-1:new.person@gmail.com:New Person", "accept_terms": true, "locale": "es" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["is_new_account"], true);
    assert_eq!(created.body["email_verified"], true);
    let token = created.body["token"].as_str().unwrap().to_string();
    let me = app.get("/users/me", &token).await;
    assert_eq!(me.body["username"], "new.person");
    assert_eq!(me.body["password_set"], false);
    assert!(
        app.last_email("welcome", Some("new.person@gmail.com"))
            .await
            .is_some()
    );

    let again = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-1:new.person@gmail.com" }),
        )
        .await;
    assert_eq!(again.body["is_new_account"], false);

    let identities = app.get("/users/me/identities", &token).await;
    assert_eq!(identities.body[0]["provider"], "google");
    // Unlinking would leave the account without a way in.
    let unlink = app.delete("/users/me/identities/google", &token).await;
    assert_eq!(unlink.code(), "PASSWORD_NOT_SET");

    // Garbage tokens are rejected.
    let invalid = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "definitely-not-a-google-token" }),
        )
        .await;
    assert_eq!(invalid.code(), "INVALID_GOOGLE_TOKEN");

    // An unverified local account with the same address is not linked.
    app.registered_user("local.user", "local@example.com").await;
    let unverified = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-2:local@example.com" }),
        )
        .await;
    assert_eq!(unverified.code(), "EMAIL_TAKEN");

    // Once verified, the same sign-in links to it.
    let local_token = app.login("local.user", STRONG_PASSWORD).await;
    app.verify_email(&local_token, "local@example.com").await;
    let linked = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-2:local@example.com" }),
        )
        .await;
    assert_eq!(linked.status, StatusCode::OK, "{}", linked.body);
    assert_eq!(linked.body["is_new_account"], false);
    let linked_token = linked.body["token"].as_str().unwrap();
    assert_eq!(
        app.get("/users/me", linked_token).await.body["username"],
        "local.user"
    );
    assert_eq!(
        app.get("/users/me/security", linked_token).await.body["has_google"],
        true
    );

    // With 2FA, Google sign-in gets the same challenge.
    let setup = app
        .post(
            "/users/me/2fa/setup",
            &local_token,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    let secret = totp::base32_decode(setup.body["secret"].as_str().unwrap()).unwrap();
    app.post(
        "/users/me/2fa/enable",
        &local_token,
        json!({ "code": totp::code_at(&secret, now_unix()) }),
    )
    .await;
    let challenged = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-2:local@example.com" }),
        )
        .await;
    assert_eq!(challenged.body["two_factor_required"], true);
    assert!(challenged.body["challenge_token"].is_string());
}

#[tokio::test]
async fn communication_preferences_drive_the_notifier() {
    let app = app!();
    let (user_id, token) = app
        .registered_user("listener", "listener@example.com")
        .await;
    app.verify_email(&token, "listener@example.com").await;

    let unknown = app
        .put(
            "/users/me/communication",
            &token,
            json!({ "categories": { "spam": { "email": true, "in_app": true } } }),
        )
        .await;
    assert_eq!(unknown.code(), "VALIDATION_ERROR");

    let updated = app
        .put(
            "/users/me/communication",
            &token,
            json!({ "categories": {
                "bands": { "email": true, "in_app": false },
                "security": { "email": false, "in_app": false }
            } }),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(updated.body["categories"]["bands"]["in_app"], false);
    assert_eq!(
        updated.body["categories"]["security"]["email"], true,
        "security is locked"
    );

    // Band notification: no in-app row, but an e-mail copy.
    notify(
        &app.state,
        Notification::band_member_added(
            user_id,
            Uuid::new_v4(),
            "Os Tais",
            setlyst_api::models::band::BandRole::Member,
            Uuid::new_v4(),
        ),
    )
    .await;
    let count = app.get("/notifications/unread-count", &token).await;
    assert_eq!(count.body["unread_count"], 0);
    let (_, payload, _) = app
        .last_email("notification", Some("listener@example.com"))
        .await
        .expect("notification e-mail");
    assert_eq!(payload["category"], "bands");
    assert!(payload["title"].as_str().unwrap().contains("Os Tais"));

    // Account notifications stay in the app by default.
    notify(
        &app.state,
        Notification::credits_granted(user_id, 10, "admin_adjustment"),
    )
    .await;
    assert_eq!(
        app.get("/notifications/unread-count", &token).await.body["unread_count"],
        1
    );
}

#[tokio::test]
async fn unsubscribe_links_switch_one_category_off() {
    let app = app!();
    let (user_id, token) = app.registered_user("reader", "reader@example.com").await;
    let link_token = unsubscribe::token(user_id, Category::Announcements);

    let inspected = app
        .request(
            Method::GET,
            &format!("/public/email/unsubscribe?token={link_token}"),
            None,
            None,
        )
        .await;
    assert_eq!(inspected.body["valid"], true);
    assert_eq!(inspected.body["category"], "announcements");

    let done = app
        .post_public("/public/email/unsubscribe", json!({ "token": link_token }))
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let prefs = app.get("/users/me/communication", &token).await;
    assert_eq!(prefs.body["categories"]["announcements"]["email"], false);
    assert_eq!(prefs.body["categories"]["announcements"]["in_app"], true);

    let forged = app
        .post_public(
            "/public/email/unsubscribe",
            json!({ "token": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }),
        )
        .await;
    assert_eq!(forged.status, StatusCode::BAD_REQUEST);
    let invalid = app
        .request(
            Method::GET,
            "/public/email/unsubscribe?token=nope",
            None,
            None,
        )
        .await;
    assert_eq!(invalid.body["valid"], false);
}

#[tokio::test]
async fn consent_profile_and_self_service_deletion() {
    let app = app!();
    let legacy = app
        .create_user("legacy.user", STRONG_PASSWORD, Role::User)
        .await;
    let token = app.login("legacy.user", STRONG_PASSWORD).await;
    let login = app.login_response("legacy.user", STRONG_PASSWORD).await;
    assert_eq!(login.body["terms_accepted"], false);

    let wrong = app
        .post(
            "/users/me/accept-terms",
            &token,
            json!({ "version": "2020-01-01" }),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    let accepted = app
        .post(
            "/users/me/accept-terms",
            &token,
            json!({ "version": "2026-09-23" }),
        )
        .await;
    assert_eq!(accepted.body["terms_version"], "2026-09-23");

    // Profile fields.
    let profile = app
        .patch(
            "/users/me",
            &token,
            json!({ "bio": "Guitarrista.", "location": "Caxias do Sul",
                    "instruments": [" Guitar ", "guitar", "Vocals"],
                    "avatar_url": "https://images.example.com/me.png" }),
        )
        .await;
    assert_eq!(profile.status, StatusCode::OK, "{}", profile.body);
    assert_eq!(profile.body["instruments"], json!(["Guitar", "Vocals"]));
    assert_eq!(
        profile.body["avatar_url"],
        "https://images.example.com/me.png"
    );
    let bad_avatar = app
        .patch(
            "/users/me",
            &token,
            json!({ "avatar_url": "http://images.example.com/me.png" }),
        )
        .await;
    assert_eq!(bad_avatar.code(), "INVALID_IMAGE_URL");
    let too_many = app
        .patch(
            "/users/me",
            &token,
            json!({ "instruments": ["a","b","c","d","e","f","g","h","i"] }),
        )
        .await;
    assert_eq!(too_many.code(), "VALIDATION_ERROR");
    let cleared = app
        .patch("/users/me", &token, json!({ "bio": "", "avatar_url": "" }))
        .await;
    assert!(cleared.body["bio"].is_null());
    assert!(cleared.body["avatar_url"].is_null());

    // Deletion needs the exact username and the password.
    let wrong_confirmation = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "confirmation": "legacy", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(wrong_confirmation.status, StatusCode::BAD_REQUEST);
    let wrong_password = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "confirmation": "legacy.user", "password": "Wr0ng!Password" })),
        )
        .await;
    assert_eq!(wrong_password.code(), "WRONG_PASSWORD");

    sqlx::query("UPDATE users SET email = 'legacy@example.com' WHERE id = $1")
        .bind(legacy)
        .execute(&app.pool)
        .await
        .unwrap();
    let deleted = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "confirmation": "legacy.user", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let (to, _, user_id) = app
        .last_email("account_deleted", None)
        .await
        .expect("goodbye e-mail");
    assert_eq!(to, "legacy@example.com");
    assert!(user_id.is_none());
    assert_eq!(
        app.login_response("legacy.user", STRONG_PASSWORD)
            .await
            .code(),
        "INVALID_CREDENTIALS"
    );
    let audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.self_deleted' AND target_id = $1",
    )
    .bind(legacy)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audit, 1);
}

/// A mail transport that fails a configurable number of times.
struct FlakyTransport {
    failures_left: std::sync::atomic::AtomicUsize,
    sent: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl setlyst_api::email::worker::MailTransport for FlakyTransport {
    async fn send(
        &self,
        to: &str,
        email: &setlyst_api::email::templates::RenderedEmail,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self
            .failures_left
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("421 try again later".into());
        }
        self.sent
            .lock()
            .unwrap()
            .push((to.to_string(), email.subject.clone()));
        Ok(())
    }
}

#[tokio::test]
async fn the_email_worker_delivers_retries_and_wipes_codes() {
    use setlyst_api::email::worker::{Delivery, process_batch};
    let app = app!();
    app.registered_user("mailer", "mailer@example.com").await;

    // Without SMTP, messages are logged and skipped, and codes wiped.
    let outcomes = process_batch(&app.pool, None, "https://setlyst.test")
        .await
        .unwrap();
    assert_eq!(outcomes, vec![Delivery::Skipped]);
    let (status, payload): (String, serde_json::Value) = sqlx::query_as(
        "SELECT status::text, payload FROM email_outbox WHERE template = 'email_verification_code'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(status, "skipped");
    assert_eq!(payload, json!({}));

    // A failing server: retried later, then delivered.
    let (user_id, token) = app
        .registered_user("mailer.two", "two.mail@example.com")
        .await;
    let transport = FlakyTransport {
        failures_left: std::sync::atomic::AtomicUsize::new(1),
        sent: std::sync::Mutex::new(Vec::new()),
    };
    let first = process_batch(&app.pool, Some(&transport), "https://setlyst.test")
        .await
        .unwrap();
    assert_eq!(first, vec![Delivery::Retrying]);
    let (attempts, error): (i32, Option<String>) =
        sqlx::query_as("SELECT attempts, last_error FROM email_outbox WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    assert!(error.unwrap().contains("421"));
    // Not due yet.
    assert!(
        process_batch(&app.pool, Some(&transport), "https://setlyst.test")
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE email_outbox SET scheduled_at = scheduled_at - INTERVAL '1 hour'")
        .execute(&app.pool)
        .await
        .unwrap();
    let second = process_batch(&app.pool, Some(&transport), "https://setlyst.test")
        .await
        .unwrap();
    assert_eq!(second, vec![Delivery::Sent]);
    let sent = transport.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "two.mail@example.com");
    assert!(sent[0].1.contains("Confirm"));

    // Cleanup jobs: old delivered mail and expired codes go away.
    sqlx::query("UPDATE email_outbox SET created_at = created_at - INTERVAL '40 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        setlyst_api::jobs::cleanup_outbox(&app.pool).await.unwrap(),
        2
    );
    sqlx::query("UPDATE verification_codes SET expires_at = expires_at - INTERVAL '3 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        setlyst_api::jobs::purge_expired_codes(&app.state)
            .await
            .unwrap(),
        2
    );
    let _ = token;
}

// ---------------------------------------------------------------------
// Attempt limits (security review fixes).
// ---------------------------------------------------------------------

/// Enables 2FA for the signed-in user; returns the secret and the
/// recovery codes.
async fn enable_two_factor(app: &TestApp, token: &str) -> (Vec<u8>, Vec<String>) {
    let setup = app
        .post(
            "/users/me/2fa/setup",
            token,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);
    let secret = totp::base32_decode(setup.body["secret"].as_str().unwrap()).unwrap();
    let enabled = app
        .post(
            "/users/me/2fa/enable",
            token,
            json!({ "code": totp::code_at(&secret, now_unix()) }),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
    let recovery = enabled.body["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    (secret, recovery)
}

async fn challenge_for(app: &TestApp, username: &str) -> String {
    let step = app.login_response(username, STRONG_PASSWORD).await;
    assert_eq!(step.status, StatusCode::OK, "{}", step.body);
    assert_eq!(step.body["two_factor_required"], true, "{}", step.body);
    step.body["challenge_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn wrong_two_factor_codes_across_fresh_sign_ins_lock_the_account() {
    let app = app!();
    let (user_id, token) = app.registered_user("bruteforced", "bf@example.com").await;
    enable_two_factor(&app, &token).await;

    let first = challenge_for(&app, "bruteforced").await;
    for left in (1..5).rev() {
        let wrong = app
            .post_public(
                "/auth/login/2fa",
                json!({ "challenge_token": first, "code": "000000" }),
            )
            .await;
        assert_eq!(wrong.code(), "INVALID_TWO_FACTOR_CODE", "{}", wrong.body);
        assert_eq!(wrong.body["meta"]["attempts_left"], left);
    }

    // A correct password again must not wipe the four failures, and the
    // new challenge replaces the old one.
    let second = challenge_for(&app, "bruteforced").await;
    let stale = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": first, "code": "000000" }),
        )
        .await;
    assert_eq!(stale.code(), "INVALID_TWO_FACTOR_CODE");
    assert_eq!(stale.body["meta"]["attempts_left"], 0);
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM login_challenges WHERE user_id = $1 AND consumed_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(live, 1, "only the newest challenge is live");

    // The fifth wrong code overall locks the account.
    let fifth = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": second, "code": "000000" }),
        )
        .await;
    assert_eq!(
        fifth.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        fifth.body
    );
    assert_eq!(fifth.code(), "ACCOUNT_LOCKED");
    let locked = app.login_response("bruteforced", STRONG_PASSWORD).await;
    assert_eq!(locked.code(), "ACCOUNT_LOCKED");
    let after_lock = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": second, "code": "000000" }),
        )
        .await;
    assert_eq!(after_lock.code(), "ACCOUNT_LOCKED");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_wrong_codes_never_exceed_the_attempt_limits() {
    let app = app!();
    let (user_id, token) = app.registered_user("racer", "racer@example.com").await;

    // E-mail verification code: 20 wrong guesses at once.
    let email_code = app
        .last_code("email_verification_code", "racer@example.com")
        .await;
    let wrong = if email_code == "000000" {
        "111111"
    } else {
        "000000"
    };
    let responses = app
        .concurrent(
            Some(&token),
            (0..20)
                .map(|_| {
                    (
                        Method::POST,
                        "/users/me/email/verify".to_string(),
                        json!({ "code": wrong }),
                    )
                })
                .collect(),
        )
        .await;
    assert!(responses.iter().all(|r| r.code() == "INVALID_CODE"));
    let attempts: i32 = sqlx::query_scalar(
        "SELECT MAX(attempts) FROM verification_codes WHERE user_id = $1 AND purpose = 'email_verification'",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 5, "attempts never exceed the maximum");
    // The code is burned: even the right one fails now.
    let burned = app.verify_email(&token, "racer@example.com").await;
    assert_eq!(burned.code(), "INVALID_CODE");

    // Sign-in challenge: 20 wrong codes at once.
    enable_two_factor(&app, &token).await;
    let challenge = challenge_for(&app, "racer").await;
    let responses = app
        .concurrent(
            None,
            (0..20)
                .map(|_| {
                    (
                        Method::POST,
                        "/auth/login/2fa".to_string(),
                        json!({ "challenge_token": challenge, "code": "000000" }),
                    )
                })
                .collect(),
        )
        .await;
    for r in &responses {
        assert!(
            matches!(r.code(), "INVALID_TWO_FACTOR_CODE" | "ACCOUNT_LOCKED"),
            "{}",
            r.body
        );
    }
    let (challenge_attempts, failures): (i32, i32) = sqlx::query_as(
        "SELECT (SELECT MAX(attempts) FROM login_challenges WHERE user_id = $1),
                (SELECT failed_login_count FROM users WHERE id = $1)",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(challenge_attempts <= 5, "{challenge_attempts} attempts");
    assert!(failures <= 5, "{failures} failures");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn second_factor_checks_outside_sign_in_are_limited() {
    let app = app!();
    let (user_id, token) = app.registered_user("guarded", "guarded@example.com").await;
    let (_, recovery) = enable_two_factor(&app, &token).await;

    for left in (0..5).rev() {
        let wrong = app
            .post(
                "/users/me/2fa/disable",
                &token,
                json!({ "password": STRONG_PASSWORD, "code": "000000" }),
            )
            .await;
        assert_eq!(wrong.code(), "INVALID_TWO_FACTOR_CODE", "{}", wrong.body);
        assert_eq!(wrong.body["meta"]["attempts_left"], left);
    }
    // Out of attempts: even a correct code waits, on both endpoints.
    let limited = app
        .post(
            "/users/me/2fa/disable",
            &token,
            json!({ "password": STRONG_PASSWORD, "code": recovery[0] }),
        )
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "TOO_MANY_ATTEMPTS");
    let retry = limited.body["meta"]["retry_after_seconds"]
        .as_i64()
        .unwrap();
    assert!(retry > 0 && retry <= 15 * 60, "{retry}");
    let regenerate = app
        .post(
            "/users/me/2fa/recovery-codes",
            &token,
            json!({ "code": recovery[0] }),
        )
        .await;
    assert_eq!(regenerate.code(), "TOO_MANY_ATTEMPTS");
    let audit = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE target_id = $1 AND action = 'user.second_factor_failed'",
            user_id,
            5,
        )
        .await;
    assert_eq!(audit, 5);

    // A new window: concurrent guesses still can't exceed five.
    sqlx::query(
        "UPDATE second_factor_attempts SET window_started_at = window_started_at - INTERVAL '16 minutes' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let responses = app
        .concurrent(
            Some(&token),
            (0..20)
                .map(|_| {
                    (
                        Method::POST,
                        "/users/me/2fa/recovery-codes".to_string(),
                        json!({ "code": "000000" }),
                    )
                })
                .collect(),
        )
        .await;
    let wrong = responses
        .iter()
        .filter(|r| r.code() == "INVALID_TWO_FACTOR_CODE")
        .count();
    let throttled = responses
        .iter()
        .filter(|r| r.code() == "TOO_MANY_ATTEMPTS")
        .count();
    assert_eq!((wrong, throttled), (5, 15));

    // After the window, a correct code works and doesn't count.
    sqlx::query(
        "UPDATE second_factor_attempts SET window_started_at = window_started_at - INTERVAL '16 minutes' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let regenerated = app
        .post(
            "/users/me/2fa/recovery-codes",
            &token,
            json!({ "code": recovery[0] }),
        )
        .await;
    assert_eq!(regenerated.status, StatusCode::OK, "{}", regenerated.body);
    let failures: i32 =
        sqlx::query_scalar("SELECT failures FROM second_factor_attempts WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(failures, 0);
}

#[tokio::test]
async fn password_reset_answers_alike_for_unknown_and_known_accounts() {
    let app = app!();
    let (user_id, _) = app
        .registered_user("resetter", "resetter@example.com")
        .await;
    let weak = json!({ "code": "123456", "new_password": "weak" });
    let reset = |identifier: &str| {
        let mut body = weak.clone();
        body["identifier"] = json!(identifier);
        body
    };

    // Without a live code, a known account and an unknown identifier give
    // the same answer, weak password or not.
    let unknown = app
        .post_public("/auth/password/reset", reset("nobody-here"))
        .await;
    let known = app
        .post_public("/auth/password/reset", reset("resetter"))
        .await;
    assert_eq!(unknown.status, known.status);
    assert_eq!(unknown.body, known.body);
    assert_eq!(known.code(), "INVALID_CODE");

    // With a live code, a wrong code is reported before the weak password.
    app.post_public("/auth/password/forgot", json!({ "identifier": "resetter" }))
        .await;
    let code = app
        .last_code("password_reset_code", "resetter@example.com")
        .await;
    let wrong_code = if code == "123456" { "654321" } else { "123456" };
    let mut body = reset("resetter");
    body["code"] = json!(wrong_code);
    let wrong = app.post_public("/auth/password/reset", body).await;
    assert_eq!(wrong.code(), "INVALID_CODE", "{}", wrong.body);
    assert_eq!(wrong.body["meta"]["attempts_left"], 4);

    // A right code with a weak password doesn't use the code up (nor an
    // attempt): the owner just picks a better password.
    for _ in 0..6 {
        let weak = app
            .post_public(
                "/auth/password/reset",
                json!({ "identifier": "resetter", "code": code, "new_password": "weak" }),
            )
            .await;
        assert_eq!(weak.code(), "WEAK_PASSWORD", "{}", weak.body);
    }
    let ok = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "resetter", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    let attempts: i32 = sqlx::query_scalar(
        "SELECT attempts FROM verification_codes WHERE user_id = $1 AND purpose = 'password_reset'",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 2, "the wrong code and the successful use");
}
