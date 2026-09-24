//! Abuse hardening: e-mail caps for every template, one-time codes that
//! survive strangers' requests, notices only to proven addresses, the
//! second-factor limiter on 2FA enrolment, no lyrics on share links and
//! the band membership limits.

mod common;

use axum::http::{Method, StatusCode};
use common::{STRONG_PASSWORD, TestApp};
use serde_json::json;
use setlyst_api::{
    email::outbox::{EMAILS_PER_ACCOUNT_PER_DAY, OTHER_EMAILS_PER_ADDRESS_PER_DAY},
    models::user::Role,
    services::account::{CODE_DAILY_LIMIT, CODE_DAILY_LIMIT_PER_NETWORK},
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

async fn forgot_from(app: &TestApp, ip: &str, identifier: &str) -> common::TestResponse {
    app.request_with_headers(
        Method::POST,
        "/auth/password/forgot",
        None,
        Some(json!({ "identifier": identifier })),
        &[("x-forwarded-for", ip)],
    )
    .await
}

async fn outbox_count(app: &TestApp, template: &str, to: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_outbox WHERE template = $1 AND LOWER(to_email) = LOWER($2)",
    )
    .bind(template)
    .bind(to)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn strangers_cannot_invalidate_or_exhaust_the_owners_recovery_code() {
    let app = app!();
    let (user_id, _) = app.registered_user("victim", "victim@example.com").await;
    let email = "victim@example.com";

    // A stranger asks for a recovery code of the account.
    let first = forgot_from(&app, "198.51.100.1", "victim").await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    let code = app.last_code("password_reset_code", email).await;

    // Another request (from anywhere) while that code is live re-sends
    // the *same* code instead of replacing it...
    app.age_codes(user_id).await;
    forgot_from(&app, "198.51.100.2", "victim").await;
    assert_eq!(outbox_count(&app, "password_reset_code", email).await, 2);
    assert_eq!(
        app.last_code("password_reset_code", email).await,
        code,
        "a live code is re-sent, never replaced"
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verification_codes
         WHERE user_id = $1 AND purpose = 'password_reset' AND consumed_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(live, 1);

    // ...so the owner's code (the same one) still works.
    let reset = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "victim", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    // Once used, the encrypted copy is gone.
    let kept: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verification_codes WHERE user_id = $1 AND code_enc IS NOT NULL",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(kept, 0);

    // One network gets a few requests a day; the account's allowance is
    // larger, so one stranger can't use it up.
    let attacker = "203.0.113.7";
    let mut refused_at = None;
    for n in 0..=CODE_DAILY_LIMIT_PER_NETWORK {
        app.age_codes(user_id).await;
        let before = outbox_count(&app, "password_reset_code", email).await;
        forgot_from(&app, attacker, "victim").await;
        let after = outbox_count(&app, "password_reset_code", email).await;
        if after == before {
            refused_at = Some(n);
            break;
        }
    }
    assert_eq!(
        refused_at,
        Some(CODE_DAILY_LIMIT_PER_NETWORK),
        "the network's allowance (the requests above came from other networks)"
    );
    // The owner, from their own network, still gets a code.
    app.age_codes(user_id).await;
    let before = outbox_count(&app, "password_reset_code", email).await;
    forgot_from(&app, "192.0.2.50", "victim").await;
    assert_eq!(
        outbox_count(&app, "password_reset_code", email).await,
        before + 1
    );
    const {
        assert!(CODE_DAILY_LIMIT > CODE_DAILY_LIMIT_PER_NETWORK);
    }
}

#[tokio::test]
async fn notices_only_go_to_proven_addresses_and_are_capped_per_mailbox() {
    let app = app!();
    // An account with someone else's address, never verified.
    let (_, token) = app.registered_user("looper", "innocent@example.com").await;
    let change =
        |current: &str, new: &str| json!({ "current_password": current, "new_password": new });

    // Changing the password of an unverified account queues no notice to
    // the (unproven) address.
    let changed = app
        .patch(
            "/users/me/password",
            &token,
            change(STRONG_PASSWORD, "An0ther!Password"),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert!(
        app.last_email("password_changed", Some("innocent@example.com"))
            .await
            .is_none(),
        "nothing but codes goes to an unverified address"
    );

    // A verified account is told, but never more than the per-mailbox cap
    // in a day, however many times the change is looped.
    let (_, token) = app.user("changer", Role::User).await;
    let email = TestApp::seeded_email("changer");
    let mut current = STRONG_PASSWORD.to_string();
    for n in 0..(OTHER_EMAILS_PER_ADDRESS_PER_DAY + 3) {
        let next = format!("L00ped!Passw0rd{n}");
        let token = app.login("changer", &current).await;
        let changed = app
            .patch("/users/me/password", &token, change(&current, &next))
            .await;
        assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
        current = next;
        sqlx::query("UPDATE reauth_attempts SET failures = 0")
            .execute(&app.pool)
            .await
            .unwrap();
    }
    drop(token);
    assert_eq!(
        outbox_count(&app, "password_changed", &email).await,
        OTHER_EMAILS_PER_ADDRESS_PER_DAY
    );
    // Codes are never held back by that cap: a recovery still gets one.
    let forgot = forgot_from(&app, "198.51.100.9", "changer").await;
    assert_eq!(forgot.status, StatusCode::ACCEPTED);
    assert_eq!(outbox_count(&app, "password_reset_code", &email).await, 1);
}

#[tokio::test]
async fn per_mailbox_caps_fold_address_variants_and_accounts_have_a_daily_cap() {
    let app = app!();
    let (user_id, _) = app.user("capped", Role::User).await;
    let email = TestApp::seeded_email("capped");

    // Variants of one Gmail mailbox share the caps.
    for (n, to) in [
        "Ana.Maria@gmail.com",
        "anamaria+x@googlemail.com",
        "ANAMARIA@GMAIL.COM",
    ]
    .iter()
    .enumerate()
    {
        let queued = setlyst_api::email::enqueue(
            &app.pool,
            &setlyst_api::email::OutgoingEmail {
                user_id: None,
                to: to.to_string(),
                locale: "en".into(),
                template: setlyst_api::email::EmailTemplate::EmailVerificationCode {
                    username: "ana".into(),
                    code: format!("00000{n}"),
                    expires_minutes: 15,
                },
            },
        )
        .await
        .unwrap();
        assert!(!queued.is_nil(), "{to}");
    }
    let canonical: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT to_canonical) FROM email_outbox WHERE template = 'email_verification_code'
         AND to_canonical LIKE 'anamaria@%'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(canonical, 1);
    for _ in 0..setlyst_api::email::outbox::CODE_EMAILS_PER_ADDRESS_PER_DAY {
        setlyst_api::email::enqueue(
            &app.pool,
            &setlyst_api::email::OutgoingEmail {
                user_id: None,
                to: "a.n.a.m.a.r.i.a@gmail.com".into(),
                locale: "en".into(),
                template: setlyst_api::email::EmailTemplate::EmailVerificationCode {
                    username: "ana".into(),
                    code: "123456".into(),
                    expires_minutes: 15,
                },
            },
        )
        .await
        .unwrap();
    }
    let sent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_outbox WHERE template = 'email_verification_code'
         AND to_canonical = 'anamaria@gmail.com'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        sent,
        setlyst_api::email::outbox::CODE_EMAILS_PER_ADDRESS_PER_DAY
    );

    // Non-code messages of one account: capped per day, all templates
    // together (each template keeps its own per-mailbox cap too).
    let kinds = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let mut queued = 0;
    for n in 0..(EMAILS_PER_ACCOUNT_PER_DAY + 5) {
        let id = setlyst_api::email::enqueue(
            &app.pool,
            &setlyst_api::email::OutgoingEmail {
                user_id: Some(user_id),
                to: email.clone(),
                locale: "en".into(),
                template: setlyst_api::email::EmailTemplate::SecurityNotice {
                    username: "capped".into(),
                    kind: kinds[(n % 8) as usize].into(),
                    detail: None,
                },
            },
        )
        .await
        .unwrap();
        if !id.is_nil() {
            queued += 1;
        }
    }
    assert_eq!(queued, OTHER_EMAILS_PER_ADDRESS_PER_DAY);
    let mut total = queued;
    for n in 0..(EMAILS_PER_ACCOUNT_PER_DAY + 5) {
        let id = setlyst_api::email::enqueue(
            &app.pool,
            &setlyst_api::email::OutgoingEmail {
                user_id: Some(user_id),
                to: format!("copy{n}@example.com"),
                locale: "en".into(),
                template: setlyst_api::email::EmailTemplate::Welcome {
                    username: "capped".into(),
                },
            },
        )
        .await
        .unwrap();
        if !id.is_nil() {
            total += 1;
        }
    }
    assert_eq!(total, EMAILS_PER_ACCOUNT_PER_DAY);
    // A code for the account is still queued.
    let id = setlyst_api::email::enqueue(
        &app.pool,
        &setlyst_api::email::OutgoingEmail {
            user_id: Some(user_id),
            to: email.clone(),
            locale: "en".into(),
            template: setlyst_api::email::EmailTemplate::PasswordResetCode {
                username: "capped".into(),
                code: "123456".into(),
                expires_minutes: 15,
            },
        },
    )
    .await
    .unwrap();
    assert!(!id.is_nil());
}

#[tokio::test]
async fn enrolling_two_factor_goes_through_the_second_factor_limiter() {
    let app = app!();
    let (_, token) = app.user("enroller", Role::User).await;
    let setup = app
        .post(
            "/users/me/2fa/setup",
            &token,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);

    for n in (1..=5).rev() {
        let wrong = app
            .post("/users/me/2fa/enable", &token, json!({ "code": "000000" }))
            .await;
        assert_eq!(wrong.code(), "INVALID_TWO_FACTOR_CODE", "{}", wrong.body);
        assert_eq!(wrong.body["meta"]["attempts_left"], n - 1);
    }
    let limited = app
        .post("/users/me/2fa/enable", &token, json!({ "code": "000000" }))
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "TOO_MANY_ATTEMPTS");
}

#[tokio::test]
async fn google_signups_with_unvouched_addresses_must_verify_them() {
    let app = app!();
    let created = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-corp:owner@startup.example:Owner", "accept_terms": true,
                    "age_confirmed": true }),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.body["is_new_account"], true);
    assert_eq!(created.body["email_verified"], false);
    let token = created.body["token"].as_str().unwrap().to_string();
    assert!(
        app.last_email("welcome", Some("owner@startup.example"))
            .await
            .is_none()
    );
    // A code was sent; verifying with it proves the address.
    let verified = app.verify_email(&token, "owner@startup.example").await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    assert_eq!(verified.body["email_verified"], true);
    assert!(
        app.last_email("welcome", Some("owner@startup.example"))
            .await
            .is_some()
    );

    // A Gmail address is vouched for by Google itself.
    let gmail = app
        .post_public(
            "/auth/oauth/google",
            json!({ "id_token": "fake:g-gmail:someone@gmail.com:Someone", "accept_terms": true,
                    "age_confirmed": true }),
        )
        .await;
    assert_eq!(gmail.status, StatusCode::OK, "{}", gmail.body);
    assert_eq!(gmail.body["email_verified"], true);
}

#[tokio::test]
async fn share_links_never_carry_lyrics() {
    let app = app!();
    let (_, user) = app.user("sharer", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let song = app
        .post(
            "/songs",
            &user,
            json!({ "title": "Inédita", "artist_id": artist, "lyrics": "verso secreto" }),
        )
        .await;
    let song_id = song.body["id"].as_str().unwrap().to_string();
    let setlist = app.setlist(&user, "Show", None).await;
    app.add_to_setlist(&user, &setlist, &song_id).await;
    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    let token = shared.body["share_token"].as_str().unwrap().to_string();
    let public = app
        .request(
            Method::GET,
            &format!("/public/setlists/{token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.status, StatusCode::OK, "{}", public.body);
    assert_eq!(public.body["songs"][0]["title"], "Inédita");
    assert!(public.body["songs"][0].get("lyrics").is_none());
    assert!(!public.body.to_string().contains("verso secreto"));
}

#[tokio::test]
async fn setting_the_role_a_member_already_has_changes_and_notifies_nothing() {
    let app = app!();
    let (_, owner) = app.user("band.owner", Role::User).await;
    let (member_id, member) = app.user("band.member", Role::User).await;
    let band = app.band(&owner, "Os Limitados").await;
    app.join_band(&owner, &member, &band, None).await;

    let same = app
        .patch(
            &format!("/bands/{band}/members/{member_id}"),
            &owner,
            json!({ "role": "member" }),
        )
        .await;
    assert_eq!(
        same.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        same.body
    );
    let notifications: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'band_role_changed'",
    )
    .bind(member_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(notifications, 0);

    let promoted = app
        .patch(
            &format!("/bands/{band}/members/{member_id}"),
            &owner,
            json!({ "role": "moderator" }),
        )
        .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    let notifications: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'band_role_changed'",
    )
    .bind(member_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(notifications, 1);
}

#[tokio::test]
async fn transferring_ownership_revokes_the_previous_owners_invites_and_leaving_withdraws_suggestions()
 {
    let app = app!();
    let (_, owner) = app.user("first.owner", Role::User).await;
    let (heir_id, heir) = app.user("heir", Role::User).await;
    let band = app.band(&owner, "Herança").await;
    app.join_band(&owner, &heir, &band, None).await;

    // The owner mints an admin invite (only an owner can).
    let invite = app
        .post(
            &format!("/bands/{band}/invites"),
            &owner,
            json!({ "role": "admin", "max_uses": 10 }),
        )
        .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    let code = invite.body["code"].as_str().unwrap().to_string();

    let transferred = app
        .post(
            &format!("/bands/{band}/transfer-ownership"),
            &owner,
            json!({ "new_owner_id": heir_id }),
        )
        .await;
    assert_eq!(transferred.status, StatusCode::OK, "{}", transferred.body);

    // The invite carries an authority the previous owner no longer has.
    let (_, newcomer) = app.user("newcomer", Role::User).await;
    let accepted = app
        .post(&format!("/invites/{code}/accept"), &newcomer, json!({}))
        .await;
    assert_eq!(accepted.code(), "INVITE_INVALID", "{}", accepted.body);

    // A member's open suggestion is withdrawn when they leave.
    let (leaver_id, leaver) = app.user("leaver", Role::User).await;
    app.join_band(&heir, &leaver, &band, None).await;
    let artist = app.artist(&leaver, "Artista").await;
    let song = app
        .post(
            "/songs",
            &leaver,
            json!({ "title": "Minha", "artist_id": artist }),
        )
        .await;
    let song_id = song.body["id"].as_str().unwrap().to_string();
    let band_id: Uuid = band.parse().unwrap();
    let suggested = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &leaver,
            json!({ "song_id": song_id }),
        )
        .await;
    assert_eq!(suggested.status, StatusCode::CREATED, "{}", suggested.body);
    let left = app
        .delete(&format!("/bands/{band}/members/{leaver_id}"), &leaver)
        .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM band_song_suggestions WHERE band_id = $1 AND suggested_by = $2",
    )
    .bind(band_id)
    .bind(leaver_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(status, "withdrawn");
}
