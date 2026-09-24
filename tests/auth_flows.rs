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

    // The age declaration is required too.
    let no_age = app
        .register(
            "ana.maria",
            "ana@example.com",
            json!({ "age_confirmed": false }),
        )
        .await;
    assert_eq!(no_age.status, StatusCode::BAD_REQUEST);
    assert_eq!(no_age.code(), "AGE_CONFIRMATION_REQUIRED");

    let created = app
        .register(
            "ana.maria",
            "Ana@Example.com",
            json!({ "locale": "pt-BR", "marketing_opt_in": true }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["email_verified"], false);
    assert_eq!(created.body["terms_version"], "2026-09-24");
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

    // The declaration and the consents are on record.
    let id: Uuid = created.body["id"].as_str().unwrap().parse().unwrap();
    let attested: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT age_attested_at FROM users WHERE id = $1")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(attested.is_some());
    let marketing: bool = sqlx::query_scalar(
        "SELECT (metadata->>'marketing_opt_in')::boolean FROM audit_logs
         WHERE action = 'user.registered' AND target_id = $1",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(marketing);
}

#[tokio::test]
async fn referrals_pay_the_friend_on_verification_and_the_referrer_on_first_payment() {
    let app = app!();
    let (_, referrer_token) = app
        .registered_user("referrer", "referrer@example.com")
        .await;
    let code = app.get("/users/me", &referrer_token).await.body["referral_code"]
        .as_str()
        .unwrap()
        .to_string();

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

    let verified = app.verify_email(&friend_token, "friend@example.com").await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    assert_eq!(verified.body["email_verified"], true);

    // The friend gets the welcome bonus at once; the referrer only once the
    // friend actually pays, so throwaway sign-ups earn nothing.
    let friend_billing = app.get("/billing/me", &friend_token).await;
    assert_eq!(friend_billing.body["credits"]["balance"], 20);
    let referrer_billing = app.get("/billing/me", &referrer_token).await;
    assert_eq!(referrer_billing.body["credits"]["balance"], 0);
    assert_eq!(referrer_billing.body["referral"]["rewarded_count"], 0);
    let still_pending = app.get("/billing/referrals", &referrer_token).await;
    assert_eq!(still_pending.body["data"][0]["status"], "pending");

    // Verifying twice is refused.
    let again = app
        .post("/users/me/email/verification", &friend_token, json!({}))
        .await;
    assert_eq!(again.code(), "EMAIL_ALREADY_VERIFIED");

    // The same mailbox under another spelling is a self-referral.
    let (_, gmail_token) = app.registered_user("self.one", "self.one@gmail.com").await;
    let own_code = app.get("/users/me", &gmail_token).await.body["referral_code"]
        .as_str()
        .unwrap()
        .to_string();
    let alias = app
        .register(
            "self.two",
            "Self.One+promo@googlemail.com",
            json!({ "referral_code": own_code }),
        )
        .await;
    assert_eq!(alias.status, StatusCode::CREATED, "{}", alias.body);
    let alias_token = app.login("self.two", STRONG_PASSWORD).await;
    app.verify_email(&alias_token, "Self.One+promo@googlemail.com")
        .await;
    let referrals = app.get("/billing/referrals", &gmail_token).await;
    assert_eq!(referrals.body["data"][0]["status"], "rejected");
    assert_eq!(
        app.get("/billing/me", &gmail_token).await.body["credits"]["balance"],
        0
    );
}

/// Signs in from a fixed client address.
async fn login_from(
    app: &TestApp,
    ip: &str,
    username: &str,
    password: &str,
) -> common::TestResponse {
    app.request_with_headers(
        Method::POST,
        "/auth/login",
        None,
        Some(json!({ "username": username, "password": password })),
        &[("x-forwarded-for", ip)],
    )
    .await
}

#[tokio::test]
async fn sign_in_by_email_and_lockout_after_repeated_failures() {
    let app = app!();
    let (user_id, _) = app.registered_user("locky", "locky@example.com").await;

    let by_email = app
        .login_response("LOCKY@example.com", STRONG_PASSWORD)
        .await;
    assert_eq!(by_email.status, StatusCode::OK, "{}", by_email.body);
    assert_eq!(by_email.body["two_factor_required"], false);
    assert_eq!(by_email.body["terms_accepted"], true);
    assert_eq!(by_email.body["email_verified"], false);

    // Five wrong passwords from one network: every answer is a plain
    // INVALID_CREDENTIALS (a lock never tells a guesser anything)...
    let attacker = "198.51.100.66";
    for attempt in 1..=5 {
        let wrong = login_from(&app, attacker, "locky", "Wr0ng!Password").await;
        assert_eq!(wrong.code(), "INVALID_CREDENTIALS", "attempt {attempt}");
    }
    let locked = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.login_locked' AND target_id = $1",
            user_id,
            1,
        )
        .await;
    assert_eq!(locked, 1);
    // ...and that network is locked out: even the right password answers
    // like a wrong one there.
    let right_from_attacker = login_from(&app, attacker, "locky", STRONG_PASSWORD).await;
    assert_eq!(right_from_attacker.code(), "INVALID_CREDENTIALS");
    // The owner, elsewhere, is not affected, and got an e-mail.
    let owner = login_from(&app, "203.0.113.10", "locky", STRONG_PASSWORD).await;
    assert_eq!(owner.status, StatusCode::OK, "{}", owner.body);
    let (_, notice, _) = app
        .last_email("security_notice", Some("locky@example.com"))
        .await
        .expect("lock notice");
    assert_eq!(notice["kind"], "login_locked");

    // 20 failures from anywhere within 15 minutes lock the account itself,
    // which only a correct password gets to see.
    for n in 0..20 {
        let ip = format!("192.0.2.{}", n + 1);
        login_from(&app, &ip, "locky", "Wr0ng!Password").await;
    }
    app.wait_for_count(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.login_locked' AND target_id = $1
                AND metadata->>'scope' = 'account'",
        user_id,
        1,
    )
    .await;
    let wrong = login_from(&app, "203.0.113.11", "locky", "Wr0ng!Password").await;
    assert_eq!(wrong.code(), "INVALID_CREDENTIALS");
    let right = login_from(&app, "203.0.113.12", "locky", STRONG_PASSWORD).await;
    assert_eq!(
        right.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        right.body
    );
    assert_eq!(right.code(), "ACCOUNT_LOCKED");
    assert!(right.body["meta"]["until"].is_string());

    // Google sign-in is not held up by the password lockout.
    // (A non-Gmail address can't link silently, so this only checks the
    // lock isn't what refuses it.)
    let google = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-locky:locky@example.com" }),
        )
        .await;
    assert_ne!(google.code(), "ACCOUNT_LOCKED");

    // Unknown accounts still look like wrong passwords, and the typed
    // identifier is audited masked.
    let unknown = app.login_response("nobody.here", STRONG_PASSWORD).await;
    assert_eq!(unknown.code(), "INVALID_CREDENTIALS");

    // Password recovery lifts every lock.
    app.post_public("/auth/password/forgot", json!({ "identifier": "locky" }))
        .await;
    let code = app
        .last_code("password_reset_code", "locky@example.com")
        .await;
    let reset = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "locky", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    let after = login_from(&app, attacker, "locky", "N3w!Password").await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
}

#[tokio::test]
async fn two_factor_authentication_full_flow() {
    let app = app!();
    let (user_id, token) = app.registered_user("secure", "secure@example.com").await;

    // Only a verified address can protect an account with 2FA.
    let unverified = app
        .post(
            "/users/me/2fa/setup",
            &token,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(unverified.status, StatusCode::FORBIDDEN);
    assert_eq!(unverified.code(), "EMAIL_NOT_VERIFIED");
    app.verify_email(&token, "secure@example.com").await;

    let no_password = app.post("/users/me/2fa/setup", &token, json!({})).await;
    assert_eq!(no_password.status, StatusCode::FORBIDDEN);
    assert_eq!(no_password.code(), "REAUTH_REQUIRED");
    assert_eq!(no_password.body["meta"]["method"], "password");
    let wrong_password = app
        .post(
            "/users/me/2fa/setup",
            &token,
            json!({ "password": "Wr0ng!Password" }),
        )
        .await;
    assert_eq!(wrong_password.code(), "WRONG_PASSWORD");

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

    // Five wrong guesses burn the code. The answers never say how many
    // attempts are left (that would tell the account exists).
    let wrong_code = if code == "999999" { "999998" } else { "999999" };
    for _ in 0..5 {
        let wrong = app
            .post_public(
                "/auth/password/reset",
                json!({ "identifier": "forgetful", "code": wrong_code, "new_password": "N3w!Password" }),
            )
            .await;
        assert_eq!(wrong.code(), "INVALID_CODE");
        assert!(wrong.body.get("meta").is_none(), "{}", wrong.body);
    }
    let failures = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.password_reset_failed' AND target_id = $1",
            user_id,
            5,
        )
        .await;
    assert_eq!(failures, 5);
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
    assert_eq!(no_password.code(), "REAUTH_REQUIRED");
    let wrong_password = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "new@example.com", "password": "Wr0ng!Password" }),
        )
        .await;
    assert_eq!(wrong_password.code(), "WRONG_PASSWORD");

    // A taken address answers like a free one: its owner gets a notice
    // (no code), and the change can't be completed.
    app.registered_user("someone", "taken@example.com").await;
    let taken = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "TAKEN@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(taken.status, StatusCode::ACCEPTED, "{}", taken.body);
    let (_, notice, _) = app
        .last_email("security_notice", Some("taken@example.com"))
        .await
        .expect("notice to the owner of the taken address");
    assert_eq!(notice["kind"], "email_in_use");
    assert!(
        app.last_email("email_change_code", Some("taken@example.com"))
            .await
            .is_none()
    );
    // The current address was told a change started.
    let (_, started_notice, _) = app
        .last_email("security_notice", Some("old@example.com"))
        .await
        .expect("notice to the current address");
    assert_eq!(started_notice["kind"], "email_change_started");
    assert_eq!(started_notice["detail"], "T***@example.com");
    let mover_id = app
        .state
        .user_repo
        .find_by_identifier("mover")
        .await
        .unwrap()
        .unwrap()
        .id;
    app.age_codes(mover_id).await;

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

    let no_age = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-1:new.person@gmail.com:New Person", "accept_terms": true }),
        )
        .await;
    assert_eq!(no_age.code(), "AGE_CONFIRMATION_REQUIRED");

    let created = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-1:new.person@gmail.com:New Person", "accept_terms": true,
                    "age_confirmed": true, "locale": "es" }),
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

    // A verified local account is linked by its (Gmail) address.
    let (_, local_token) = app
        .registered_user("local.user", "local.user@gmail.com")
        .await;
    app.verify_email(&local_token, "local.user@gmail.com").await;
    let linked = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-2:local.user@gmail.com" }),
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
            json!({ "id_token": "fake:g-2:local.user@gmail.com" }),
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
            json!({ "version": "2026-09-24" }),
        )
        .await;
    assert_eq!(accepted.body["terms_version"], "2026-09-24");

    // Profile fields. A new avatar needs a verified address.
    let unverified_avatar = app
        .patch(
            "/users/me",
            &token,
            json!({ "avatar_url": "https://images.example.com/me.png" }),
        )
        .await;
    assert_eq!(unverified_avatar.code(), "EMAIL_NOT_VERIFIED");
    sqlx::query(
        "UPDATE users SET email = 'legacy@example.com', email_verified_at = NOW() WHERE id = $1",
    )
    .bind(legacy)
    .execute(&app.pool)
    .await
    .unwrap();
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
    let me = app.get("/users/me", token).await;
    if me.body["email_verified"] != true {
        let email = me.body["email"].as_str().unwrap().to_string();
        let verified = app.verify_email(token, &email).await;
        assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    }
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
    // A new code (2FA needs a verified address).
    app.age_codes(user_id).await;
    let resent = app
        .post("/users/me/email/verification", &token, json!({}))
        .await;
    assert_eq!(resent.status, StatusCode::ACCEPTED, "{}", resent.body);

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
    assert_eq!(wrong.body, unknown.body, "no attempts count");

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

// ---------------------------------------------------------------------
// Launch hardening: step-up re-authentication, failure limits, Google
// linking, squatting, trials.
// ---------------------------------------------------------------------

/// A Google-only account (no password); returns its id and token.
async fn google_account(app: &TestApp, subject: &str, email: &str) -> (Uuid, String) {
    let created = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": format!("fake:{subject}:{email}"), "accept_terms": true, "age_confirmed": true }),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    let token = created.body["token"].as_str().unwrap().to_string();
    let id = app.get("/users/me", &token).await.body["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (id, token)
}

#[tokio::test]
async fn accounts_without_a_password_confirm_sensitive_actions_with_an_emailed_code() {
    let app = app!();
    let (user_id, token) = google_account(&app, "g-reauth", "reauth.user@gmail.com").await;

    // No proof at all: the client is told which one to ask for.
    let missing = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "moved@example.com" }),
        )
        .await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    assert_eq!(missing.code(), "REAUTH_REQUIRED");
    assert_eq!(missing.body["meta"]["method"], "email_code");
    for (path, body) in [
        ("/users/me/2fa/setup", json!({})),
        (
            "/users/me/identities/google",
            json!({ "id_token": "fake:g-other:other@gmail.com" }),
        ),
    ] {
        let refused = app.post(path, &token, body).await;
        assert_eq!(
            refused.code(),
            "REAUTH_REQUIRED",
            "{path}: {}",
            refused.body
        );
    }
    let delete = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "confirmation": "reauth.user" })),
        )
        .await;
    assert_eq!(delete.code(), "REAUTH_REQUIRED");

    // The code goes to the verified address, at most once a minute.
    let sent = app.post("/users/me/reauth/code", &token, json!({})).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.body);
    assert_eq!(sent.body, json!({}));
    let again = app.post("/users/me/reauth/code", &token, json!({})).await;
    assert_eq!(again.status, StatusCode::TOO_MANY_REQUESTS);
    let code = app.last_code("reauth_code", "reauth.user@gmail.com").await;
    let wrong = if code == "000000" { "111111" } else { "000000" };

    let bad = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "moved@example.com", "reauth_code": wrong }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.code(), "INVALID_CODE");
    let audited = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.reauth_failed' AND target_id = $1",
            user_id,
            1,
        )
        .await;
    assert_eq!(audited, 1);

    let ok = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "moved@example.com", "reauth_code": code }),
        )
        .await;
    assert_eq!(ok.status, StatusCode::ACCEPTED, "{}", ok.body);
    // Single use.
    app.age_codes(user_id).await;
    let reused = app
        .post(
            "/users/me/email/change",
            &token,
            json!({ "new_email": "moved@example.com", "reauth_code": code }),
        )
        .await;
    assert_eq!(reused.code(), "INVALID_CODE");

    // Accounts with a password use it (and with 2FA, a code too).
    let (_, pw_token) = app.registered_user("pw.user", "pw.user@example.com").await;
    let (secret, _) = enable_two_factor(&app, &pw_token).await;
    let no_second_factor = app
        .post(
            "/users/me/email/change",
            &pw_token,
            json!({ "new_email": "pw.new@example.com", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(no_second_factor.code(), "INVALID_TWO_FACTOR_CODE");
    let with_code = app
        .post(
            "/users/me/email/change",
            &pw_token,
            json!({ "new_email": "pw.new@example.com", "password": STRONG_PASSWORD,
                    "code": totp::code_at(&secret, now_unix() + 30) }),
        )
        .await;
    assert_eq!(with_code.status, StatusCode::ACCEPTED, "{}", with_code.body);
    let method = app
        .post(
            "/users/me/2fa/disable",
            &pw_token,
            json!({ "code": "123456" }),
        )
        .await;
    assert_eq!(method.body["meta"]["method"], "password");
}

#[tokio::test]
async fn wrong_confirmation_passwords_are_limited_and_eventually_sign_out() {
    let app = app!();
    let (user_id, token) = app.registered_user("oracle", "oracle@example.com").await;
    let change =
        |current: &str| json!({ "current_password": current, "new_password": "N3w!Password" });

    for _ in 0..5 {
        let wrong = app
            .patch("/users/me/password", &token, change("Wr0ng!Password"))
            .await;
        assert_eq!(wrong.code(), "WRONG_PASSWORD", "{}", wrong.body);
    }
    // The limiter now refuses even the right password.
    let limited = app
        .patch("/users/me/password", &token, change(STRONG_PASSWORD))
        .await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.body
    );
    assert_eq!(limited.code(), "TOO_MANY_ATTEMPTS");
    assert_eq!(
        app.wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.reauth_failed' AND target_id = $1",
            user_id,
            5,
        )
        .await,
        5
    );

    // Ten wrong confirmations in a day: the session is in the wrong
    // hands, so every session is signed out and the owner is told.
    sqlx::query(
        "UPDATE reauth_attempts SET window_started_at = window_started_at - INTERVAL '16 minutes' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&app.pool)
    .await
    .unwrap();
    for _ in 0..5 {
        app.patch("/users/me/password", &token, change("Wr0ng!Password"))
            .await;
    }
    assert_eq!(app.get("/users/me", &token).await.code(), "SESSION_REVOKED");
    let (_, notice, _) = app
        .last_email("security_notice", Some("oracle@example.com"))
        .await
        .expect("sign-out notice");
    assert_eq!(notice["kind"], "reauth_sessions_revoked");
}

#[tokio::test]
async fn wrong_codes_are_capped_per_account_across_fresh_codes() {
    let app = app!();
    let (user_id, _) = app.registered_user("capped", "capped@example.com").await;
    let forgot = || app.post_public("/auth/password/forgot", json!({ "identifier": "capped" }));
    let codes_sent = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM email_outbox WHERE template = 'password_reset_code' AND user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
    };

    for round in 0..2 {
        app.age_codes(user_id).await;
        forgot().await;
        assert_eq!(codes_sent().await, round + 1);
        let code = app
            .last_code("password_reset_code", "capped@example.com")
            .await;
        let wrong = if code == "999999" { "999998" } else { "999999" };
        for _ in 0..5 {
            let answer = app
                .post_public(
                    "/auth/password/reset",
                    json!({ "identifier": "capped", "code": wrong, "new_password": "N3w!Password" }),
                )
                .await;
            assert_eq!(answer.code(), "INVALID_CODE", "{}", answer.body);
        }
    }

    // Ten wrong codes in a day: no new code, and nothing is checked.
    app.age_codes(user_id).await;
    forgot().await;
    assert_eq!(codes_sent().await, 2, "no third code");
    let refused = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "capped", "code": "123456", "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.code(), "TOO_MANY_ATTEMPTS");
}

async fn identity_count(app: &TestApp, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM oauth_identities WHERE user_id = $1")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn google_links_only_addresses_it_vouches_for_and_only_after_the_second_factor() {
    let app = app!();

    // A Workspace-less custom domain: the owner links from the settings.
    let (corp_id, corp) = app
        .registered_user("corp.user", "ceo@startup.example")
        .await;
    app.verify_email(&corp, "ceo@startup.example").await;
    let refused = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-corp:ceo@startup.example" }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "ACCOUNT_LINK_REQUIRED");
    let linked = app
        .post(
            "/users/me/identities/google",
            &corp,
            json!({ "id_token": "fake:g-corp:ceo@startup.example", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(linked.status, StatusCode::NO_CONTENT, "{}", linked.body);
    let (_, notice, _) = app
        .last_email("security_notice", Some("ceo@startup.example"))
        .await
        .expect("link notice");
    assert_eq!(notice["kind"], "google_linked");
    let signed_in = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-corp:ceo@startup.example" }),
        )
        .await;
    assert_eq!(signed_in.status, StatusCode::OK, "{}", signed_in.body);
    let unlinked = app
        .request(
            Method::DELETE,
            "/users/me/identities/google",
            Some(&corp),
            Some(json!({ "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(unlinked.status, StatusCode::NO_CONTENT, "{}", unlinked.body);
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE target_id = $1
         AND action IN ('user.identity_linked', 'user.identity_unlinked')",
    )
    .bind(corp_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);

    // A matching Workspace domain is authoritative.
    let (_, ws) = app.registered_user("ws.user", "ana@studio.example").await;
    app.verify_email(&ws, "ana@studio.example").await;
    let workspace = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-ws:ana@studio.example:@hd=studio.example" }),
        )
        .await;
    assert_eq!(workspace.status, StatusCode::OK, "{}", workspace.body);
    assert!(workspace.body["token"].is_string());

    // With 2FA, the identity is linked only once the challenge succeeds.
    let (twofa_id, twofa) = app
        .registered_user("twofa.user", "twofa.user@gmail.com")
        .await;
    let (secret, _) = enable_two_factor(&app, &twofa).await;
    let challenged = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-2fa:twofa.user@gmail.com" }),
        )
        .await;
    assert_eq!(challenged.body["two_factor_required"], true);
    assert_eq!(
        identity_count(&app, twofa_id).await,
        0,
        "nothing linked before the 2FA"
    );
    let completed = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenged.body["challenge_token"],
                    "code": totp::code_at(&secret, now_unix() + 30) }),
        )
        .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    assert_eq!(identity_count(&app, twofa_id).await, 1);

    // A suspended account gets no link.
    let (banned_id, banned) = app
        .registered_user("banned.user", "banned.user@gmail.com")
        .await;
    app.verify_email(&banned, "banned.user@gmail.com").await;
    sqlx::query("UPDATE users SET banned_at = NOW() WHERE id = $1")
        .bind(banned_id)
        .execute(&app.pool)
        .await
        .unwrap();
    let suspended = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-banned:banned.user@gmail.com" }),
        )
        .await;
    assert_eq!(suspended.code(), "ACCOUNT_BANNED");
    assert_eq!(identity_count(&app, banned_id).await, 0);
}

#[tokio::test]
async fn unverified_squatters_lose_the_address_and_whatever_they_set_up() {
    let app = app!();

    // A squatter registered the victim's address (and, from before 2FA
    // needed a verified address, turned 2FA on and linked a Google
    // account).
    let (squatter_id, _) = app.registered_user("squatter", "victim@gmail.com").await;
    let secret = setlyst_api::utils::crypto::encrypt(&[7u8; 20]).unwrap();
    sqlx::query("UPDATE users SET totp_secret_enc = $2, totp_enabled_at = NOW() WHERE id = $1")
        .bind(squatter_id)
        .bind(&secret)
        .execute(&app.pool)
        .await
        .unwrap();
    app.state
        .security_repo
        .link_identity(
            squatter_id,
            "google",
            "g-squatter",
            Some("squatter@gmail.com"),
        )
        .await
        .unwrap();

    // The real owner recovers the account through the mailbox: the first
    // proof of ownership wipes what was set up before it.
    app.post_public(
        "/auth/password/forgot",
        json!({ "identifier": "victim@gmail.com" }),
    )
    .await;
    let code = app
        .last_code("password_reset_code", "victim@gmail.com")
        .await;
    let reset = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "victim@gmail.com", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    let (two_factor, identities): (bool, i64) = sqlx::query_as(
        "SELECT totp_enabled_at IS NOT NULL,
                (SELECT COUNT(*) FROM oauth_identities WHERE user_id = $1)
         FROM users WHERE id = $1",
    )
    .bind(squatter_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(!two_factor);
    assert_eq!(identities, 0);
    let login = app.login_response("victim@gmail.com", "N3w!Password").await;
    assert!(login.body["token"].is_string(), "{}", login.body);

    // Google proves ownership too: the unverified account loses the
    // address and the owner gets a fresh account.
    let (other_squatter, _) = app
        .registered_user("squatter.two", "real.owner@gmail.com")
        .await;
    let created = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-owner:real.owner@gmail.com", "accept_terms": true, "age_confirmed": true }),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["is_new_account"], true);
    let detached: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(other_squatter)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(detached.is_none());
    assert_eq!(
        app.wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.email_detached' AND target_id = $1",
            other_squatter,
            1,
        )
        .await,
        1
    );
}

#[tokio::test]
async fn unverified_accounts_without_content_are_purged_after_a_week() {
    let app = app!();
    let (idle, _) = app
        .registered_user("idle.account", "idle@example.com")
        .await;
    let (busy, busy_token) = app
        .registered_user("busy.account", "busy@example.com")
        .await;
    app.song_id(&busy_token, "Artist", "Kept").await;
    let (verified, verified_token) = app
        .registered_user("verified.account", "verified@example.com")
        .await;
    app.verify_email(&verified_token, "verified@example.com")
        .await;
    let (recent, _) = app
        .registered_user("recent.account", "recent@example.com")
        .await;
    for id in [idle, busy, verified] {
        sqlx::query(
            "UPDATE users SET created_at = created_at - INTERVAL '8 days', last_login_at = NULL WHERE id = $1",
        )
        .bind(id)
        .execute(&app.pool)
        .await
        .unwrap();
    }

    let purged = setlyst_api::jobs::accounts::purge_unverified_accounts(&app.state)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    for (id, exists) in [
        (idle, false),
        (busy, true),
        (verified, true),
        (recent, true),
    ] {
        assert_eq!(
            app.state.user_repo.find_by_id(id).await.unwrap().is_some(),
            exists,
            "{id}"
        );
    }
    // The name stays reserved for a while.
    let reuse = app
        .register("idle.account", "someone.new@example.com", json!({}))
        .await;
    assert_eq!(reuse.code(), "USERNAME_TAKEN");
}

#[tokio::test]
async fn trials_start_on_verification_once_per_address() {
    let app = app!();
    app.set_billing(json!({ "enforced": true, "trial_plan": "pro", "trial_days": 14 }))
        .await;

    let (_, token) = app.registered_user("trialist", "tri.alist@gmail.com").await;
    let before = app.get("/billing/me", &token).await;
    assert!(before.body["subscription"].is_null(), "{}", before.body);
    app.verify_email(&token, "tri.alist@gmail.com").await;
    let after = app.get("/billing/me", &token).await;
    assert_eq!(after.body["subscription"]["status"], "trialing");

    // Deleting the account and signing up again with a variant of the
    // same Gmail address doesn't bring a second trial.
    let deleted = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&token),
            Some(json!({ "confirmation": "trialist", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let (_, again) = app
        .registered_user("trialist.two", "trialist+again@googlemail.com")
        .await;
    app.verify_email(&again, "trialist+again@googlemail.com")
        .await;
    let second = app.get("/billing/me", &again).await;
    assert!(second.body["subscription"].is_null(), "{}", second.body);

    // Google sign-ups get theirs at once.
    let (_, google) = google_account(&app, "g-trial", "fresh.trial@gmail.com").await;
    assert_eq!(
        app.get("/billing/me", &google).await.body["subscription"]["status"],
        "trialing"
    );
}

#[tokio::test]
async fn credential_changes_invalidate_pending_challenges_and_codes() {
    let app = app!();
    let (_, token) = app.registered_user("pending", "pending@example.com").await;
    let (secret, _) = enable_two_factor(&app, &token).await;
    let challenge = challenge_for(&app, "pending").await;

    let revoked = app
        .post("/users/me/sessions/revoke", &token, json!({}))
        .await;
    assert_eq!(revoked.status, StatusCode::OK);
    let stale = app
        .post_public(
            "/auth/login/2fa",
            json!({ "challenge_token": challenge, "code": totp::code_at(&secret, now_unix() + 30) }),
        )
        .await;
    assert_eq!(stale.code(), "INVALID_TWO_FACTOR_CODE", "{}", stale.body);
}

#[tokio::test]
async fn vacated_usernames_and_look_alikes_are_reserved() {
    let app = app!();
    let (owner_id, owner) = app
        .registered_user("old.name", "old.name@example.com")
        .await;
    let renamed = app
        .patch("/users/me", &owner, json!({ "username": "new.name" }))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);

    let taken = app
        .register("old.name", "other@example.com", json!({}))
        .await;
    assert_eq!(taken.code(), "USERNAME_TAKEN");
    // ...except for its previous owner.
    assert!(
        app.state
            .user_repo
            .is_username_available("old.name", Some(owner_id))
            .await
            .unwrap()
    );

    let look_alike = app.register("supp0rt", "fake@example.com", json!({})).await;
    assert_eq!(look_alike.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn avatar_changes_need_a_verified_address_and_are_limited() {
    let app = app!();
    let (_, token) = app
        .registered_user("pictured", "pictured@example.com")
        .await;
    app.verify_email(&token, "pictured@example.com").await;
    for n in 0..5 {
        let set = app
            .patch(
                "/users/me",
                &token,
                json!({ "avatar_url": format!("https://img.example.com/{n}.png") }),
            )
            .await;
        assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    }
    // The same picture with another query string costs nothing.
    let same = app
        .patch(
            "/users/me",
            &token,
            json!({ "avatar_url": "https://img.example.com/4.png?v=2" }),
        )
        .await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.body);
    let sixth = app
        .patch(
            "/users/me",
            &token,
            json!({ "avatar_url": "https://img.example.com/new.png" }),
        )
        .await;
    assert_eq!(
        sixth.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        sixth.body
    );
    assert_eq!(sixth.code(), "TOO_MANY_ATTEMPTS");
}
