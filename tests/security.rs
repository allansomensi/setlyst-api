//! Platform security fixes (v0.12): session binding of impersonation,
//! JWT validation, merged UI settings bounds, pagination clamps, the
//! status split, cache headers and the last-admin guard.

mod common;

use axum::http::{Method, StatusCode};
use common::{STRONG_PASSWORD, TestApp};
use serde_json::json;
use setlyst_api::models::user::Role;

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn impersonation_ends_when_the_impersonator_is_signed_out() {
    let app = app!();
    let (admin_id, admin) = app.user("imp.admin", Role::Admin).await;
    let (target_id, _) = app.user("imp.target", Role::User).await;

    let issued = app
        .post(
            &format!("/users/{target_id}/impersonate"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let token = issued.body["token"].as_str().unwrap().to_string();
    assert_eq!(
        app.get("/users/me", &token).await.body["username"],
        "imp.target"
    );
    let verified = app
        .post_public("/auth/verify", json!({ "token": token }))
        .await;
    assert_eq!(verified.status, StatusCode::OK);

    // "Sign out everywhere" for the admin also ends the "view as" session.
    app.state.user_repo.revoke_sessions(admin_id).await.unwrap();
    assert_eq!(app.get("/users/me", &token).await.code(), "SESSION_REVOKED");
    let verified = app
        .post_public("/auth/verify", json!({ "token": token }))
        .await;
    assert_eq!(verified.code(), "SESSION_REVOKED");
}

#[tokio::test]
async fn tokens_issued_in_the_future_or_with_other_algorithms_are_rejected() {
    let app = app!();
    let (user_id, _) = app.user("clock.user", Role::User).await;
    let secret = "integration-tests-secret-that-is-long-enough";
    let now = chrono::Utc::now().timestamp();
    let claims = |iat: i64| {
        json!({ "sub": user_id, "username": "clock.user", "role": "user", "status": "active",
                "exp": now + 3600, "iat": iat, "ver": 0 })
    };
    let sign = |alg: jsonwebtoken::Algorithm, iat: i64| {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(alg),
            &claims(iat),
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    };
    let good = sign(jsonwebtoken::Algorithm::HS256, now);
    assert_eq!(app.get("/users/me", &good).await.status, StatusCode::OK);
    let future = sign(jsonwebtoken::Algorithm::HS256, now + 3600);
    assert_eq!(
        app.get("/users/me", &future).await.status,
        StatusCode::UNAUTHORIZED
    );
    let other_alg = sign(jsonwebtoken::Algorithm::HS512, now);
    assert_eq!(
        app.get("/users/me", &other_alg).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn merged_ui_settings_are_bounded() {
    let app = app!();
    let (_, user) = app.user("settings.user", Role::User).await;
    let half = "x".repeat(9 * 1024);

    let first = app
        .patch(
            "/users/me/preferences",
            &user,
            json!({ "ui_settings": { "a": half } }),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    // Each patch is small enough on its own; together they are too big.
    let second = app
        .patch(
            "/users/me/preferences",
            &user,
            json!({ "ui_settings": { "b": half } }),
        )
        .await;
    assert_eq!(second.status, StatusCode::BAD_REQUEST);
    assert_eq!(second.code(), "VALIDATION_ERROR");
    assert!(second.body["meta"]["fields"]["ui_settings"].is_array());

    let deep = app
        .patch(
            "/users/me/preferences",
            &user,
            json!({ "ui_settings": { "a": { "b": { "c": { "d": { "e": { "f": { "g": 1 } } } } } } } }),
        )
        .await;
    assert_eq!(deep.code(), "VALIDATION_ERROR");

    let stored = app.get("/users/me/preferences", &user).await;
    assert!(stored.body["ui_settings"].get("b").is_none());
}

#[tokio::test]
async fn huge_page_numbers_are_clamped() {
    let app = app!();
    // (The audit log is admin-only.)
    let (_, moderator) = app.user("pager", Role::Admin).await;
    for path in [
        "/users?page=9223372036854775807&per_page=100",
        "/notifications?page=9223372036854775807",
        "/admin/audit-logs?page=9223372036854775807",
        "/admin/songs?page=9223372036854775807",
        "/admin/moderation/flags?page=9223372036854775807",
        "/announcements?page=9223372036854775807",
    ] {
        let response = app.get(path, &moderator).await;
        assert_eq!(response.status, StatusCode::OK, "{path}: {}", response.body);
        assert_eq!(
            response.body["meta"]["current_page"],
            setlyst_api::models::MAX_PAGE,
            "{path}"
        );
    }
}

#[tokio::test]
async fn status_details_are_staff_only_and_sessions_are_never_cached() {
    let app = app!();
    let (_, user) = app.user("status.user", Role::User).await;
    let (_, moderator) = app.user("status.staff", Role::Moderator).await;

    let public = app.request(Method::GET, "/status", None, None).await;
    assert_eq!(public.status, StatusCode::OK);
    assert_eq!(
        public.body.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["status"]
    );
    assert_eq!(
        app.request(Method::GET, "/status/details", None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get("/status/details", &user).await.status,
        StatusCode::FORBIDDEN
    );
    let details = app.get("/status/details", &moderator).await;
    assert_eq!(details.status, StatusCode::OK);
    assert!(details.body["dependencies"]["database"]["version"].is_string());

    let me = app.get("/users/me", &user).await;
    assert_eq!(me.headers["cache-control"], "no-store");
    let legal = app
        .request(Method::GET, "/public/legal/version", None, None)
        .await;
    assert_eq!(legal.body["version"], "2026-09-24");
}

#[tokio::test]
async fn the_last_admin_can_neither_leave_nor_be_demoted() {
    let app = app!();
    let (first_id, first) = app.user("first.admin", Role::Admin).await;

    let leaving = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&first),
            Some(json!({ "confirmation": "first.admin", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(leaving.status, StatusCode::CONFLICT);
    assert_eq!(leaving.code(), "LAST_ADMIN");

    // Admins can't change each other's role through the API (a rogue
    // admin could demote a peer and then take the account over): that is
    // the CLI's job, and the last-admin guard still holds there.
    let (_, second) = app.user("second.admin", Role::Admin).await;
    let demoted = app
        .patch(
            &format!("/users/{first_id}"),
            &second,
            json!({ "role": "user" }),
        )
        .await;
    assert_eq!(demoted.status, StatusCode::FORBIDDEN, "{}", demoted.body);
    assert_eq!(demoted.code(), "INSUFFICIENT_ROLE");
    for forbidden in [
        app.delete(&format!("/users/{first_id}"), &second).await,
        app.post(
            &format!("/users/{first_id}/password-reset"),
            &second,
            json!({ "new_password": "Temp#Pass2026" }),
        )
        .await,
        app.post(
            &format!("/users/{first_id}/impersonate"),
            &second,
            json!({}),
        )
        .await,
    ] {
        assert_eq!(forbidden.code(), "INSUFFICIENT_ROLE", "{}", forbidden.body);
    }

    // With two admins, one can leave; the last one can't.
    let left = app
        .request(
            Method::DELETE,
            "/users/me",
            Some(&second),
            Some(json!({ "confirmation": "second.admin", "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
    let admins = app.state.user_repo.count_active_admins().await.unwrap();
    assert_eq!(admins, 1);
    // The repository guard (used by the CLI demotion) keeps the last one.
    let demote_last = app
        .state
        .user_repo
        .update(
            first_id,
            &setlyst_api::models::user::UpdateUserPayload {
                role: Some(Role::User),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(demote_last.code(), "LAST_ADMIN");
}

#[tokio::test]
async fn failed_sign_ins_of_unknown_accounts_are_audited() {
    let app = app!();
    let response = app.login_response("ghost.account", STRONG_PASSWORD).await;
    assert_eq!(response.code(), "INVALID_CREDENTIALS");
    let mut found = 0i64;
    for _ in 0..50 {
        found = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.login_failed'
             AND target_id IS NULL AND target_label LIKE '#%' AND target_label <> 'ghost.account'",
        )
        .fetch_one(&app.pool)
        .await
        .unwrap();
        if found > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(found, 1);
}

#[tokio::test]
async fn signing_out_everywhere_revokes_every_session_of_the_caller() {
    let app = app!();
    let (user_id, first) = app.user("everywhere.user", Role::User).await;
    let second = app.login("everywhere.user", STRONG_PASSWORD).await;
    let (_, other) = app.user("everywhere.other", Role::User).await;

    let revoked = app
        .post("/users/me/sessions/revoke", &first, json!({}))
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body, json!({ "reauth_required": true }));

    // Both devices are signed out; other accounts are untouched.
    assert_eq!(app.get("/users/me", &first).await.code(), "SESSION_REVOKED");
    assert_eq!(
        app.get("/users/me", &second).await.code(),
        "SESSION_REVOKED"
    );
    assert_eq!(app.get("/users/me", &other).await.status, StatusCode::OK);
    let fresh = app.login("everywhere.user", STRONG_PASSWORD).await;
    assert_eq!(app.get("/users/me", &fresh).await.status, StatusCode::OK);

    let (actor, target): (Option<uuid::Uuid>, Option<uuid::Uuid>) = sqlx::query_as(
        "SELECT actor_id, target_id FROM audit_logs WHERE action = 'user.sessions_revoked'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(actor, Some(user_id));
    assert_eq!(target, Some(user_id));
}

#[tokio::test]
async fn staff_viewing_an_account_cannot_bulk_export_its_content() {
    let app = app!();
    let (_, admin) = app.user("exp.admin", Role::Admin).await;
    let (target_id, target) = app.user("exp.target", Role::User).await;
    let setlist = app.setlist(&target, "Private", None).await;

    let issued = app
        .post(
            &format!("/users/{target_id}/impersonate"),
            &admin,
            json!({ "reason": "Support ticket" }),
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let token = issued.body["token"].as_str().unwrap().to_string();

    let song = app.song_id(&target, "Private Artist", "Private Song").await;
    for path in [
        "/backup/export".to_string(),
        "/songs/export/chordpro".to_string(),
        format!("/setlists/{setlist}/export/pdf"),
        // One song at a time is still their content.
        format!("/songs/{song}/export/chordpro"),
        format!("/songs/{song}/export/pdf"),
    ] {
        let response = app.get(&path, &token).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(response.code(), "IMPERSONATION_READ_ONLY", "{path}");
    }
    // The account itself still can.
    assert_eq!(
        app.get("/backup/export", &target).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn staff_title_edits_that_collide_answer_conflict() {
    let app = app!();
    let (_, admin) = app.user("dup.curator", Role::Admin).await;
    let (_, user) = app.user("dup.musician", Role::User).await;
    let artist = app.artist(&user, "Same Artist").await;
    app.song(&user, &artist, "Taken").await;
    let other = app.song(&user, &artist, "Other").await;
    let other_id = other.body["id"].as_str().unwrap();

    let collision = app
        .patch(
            &format!("/admin/songs/{other_id}"),
            &admin,
            json!({ "title": "Taken" }),
        )
        .await;
    assert_eq!(collision.status, StatusCode::CONFLICT, "{}", collision.body);
}

// ---------------------------------------------------------------------
// Launch hardening: impersonation secrets, staff 2FA, staff powers.
// ---------------------------------------------------------------------

#[tokio::test]
async fn impersonation_withholds_secrets_blocks_exports_and_is_audited() {
    let app = app!();
    let (_, admin) = app.user("imp.viewer", Role::Admin).await;
    let (target_id, target) = app.user("imp.owner", Role::User).await;
    let setlist = app.setlist(&target, "Shared set", None).await;
    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &target, json!({}))
        .await;
    assert!(shared.body["share_token"].is_string(), "{}", shared.body);
    let band = app.band(&target, "Private band").await;
    let invite = app
        .post(&format!("/bands/{band}/invites"), &target, json!({}))
        .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);

    let issued = app
        .post(
            &format!("/users/{target_id}/impersonate"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let token = issued.body["token"].as_str().unwrap().to_string();

    // The owner still sees everything; the "view as" session doesn't.
    let own = app.get(&format!("/setlists/{setlist}"), &target).await;
    assert!(own.body["share_token"].is_string());
    let viewed = app.get(&format!("/setlists/{setlist}"), &token).await;
    assert_eq!(viewed.status, StatusCode::OK, "{}", viewed.body);
    assert!(viewed.body["share_token"].is_null(), "{}", viewed.body);
    let invites = app.get(&format!("/bands/{band}/invites"), &token).await;
    assert_eq!(invites.status, StatusCode::OK, "{}", invites.body);
    let text = invites.body.to_string();
    assert!(text.contains("\"***\""), "{text}");
    assert!(
        !text.contains(invite.body["code"].as_str().unwrap()),
        "{text}"
    );

    for path in [
        "/backup/export",
        "/songs/export/chordpro",
        "/users/me/data-export",
    ] {
        let refused = app.get(path, &token).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(refused.code(), "IMPERSONATION_READ_ONLY", "{path}");
    }
    let pdf = app
        .get(&format!("/setlists/{setlist}/export/pdf"), &token)
        .await;
    assert_eq!(pdf.code(), "IMPERSONATION_READ_ONLY");

    let blocked = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.impersonated_read'
             AND target_id = $1 AND (metadata->>'blocked')::boolean",
            target_id,
            4,
        )
        .await;
    assert_eq!(blocked, 4);
}

#[tokio::test]
async fn staff_must_enable_two_factor_after_the_grace_period() {
    let app = app!();
    let (admin_id, admin) = app.user("strict.admin", Role::Admin).await;
    // Fresh staff get a short grace period to sign in and enrol.
    assert_eq!(app.get("/users", &admin).await.status, StatusCode::OK);

    sqlx::query(
        "UPDATE users SET role_changed_at = NOW() AT TIME ZONE 'utc' - INTERVAL '2 hours',
                email = 'strict.admin@example.com', email_verified_at = NOW()
         WHERE id = $1",
    )
    .bind(admin_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let blocked = app.get("/users", &admin).await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN);
    assert_eq!(blocked.code(), "STAFF_TWO_FACTOR_REQUIRED");
    // Their own settings stay reachable.
    assert_eq!(app.get("/users/me", &admin).await.status, StatusCode::OK);
    assert_eq!(
        app.get("/users/me/security", &admin).await.status,
        StatusCode::OK
    );

    let setup = app
        .post(
            "/users/me/2fa/setup",
            &admin,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);
    let secret =
        setlyst_api::utils::totp::base32_decode(setup.body["secret"].as_str().unwrap()).unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    let enabled = app
        .post(
            "/users/me/2fa/enable",
            &admin,
            json!({ "code": setlyst_api::utils::totp::code_at(&secret, now) }),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
    assert_eq!(app.get("/users", &admin).await.status, StatusCode::OK);
}

#[tokio::test]
async fn moderators_handle_content_but_not_takeovers_or_mass_mail() {
    let app = app!();
    let (_, admin) = app.user("power.admin", Role::Admin).await;
    let (_, moderator) = app.user("limited.mod", Role::Moderator).await;
    let (user_id, customer) = app
        .registered_user("customer.one", "customer.one@example.com")
        .await;
    // Security notices only go to a proven address.
    app.verify_email(&customer, "customer.one@example.com")
        .await;

    for (response, what) in [
        (
            app.delete(&format!("/users/{user_id}"), &moderator).await,
            "delete",
        ),
        (
            app.post(
                &format!("/users/{user_id}/password-reset"),
                &moderator,
                json!({ "new_password": "Temp#Pass2026" }),
            )
            .await,
            "password reset",
        ),
        (
            app.post(
                &format!("/users/{user_id}/impersonate"),
                &moderator,
                json!({}),
            )
            .await,
            "impersonation",
        ),
        (
            app.patch(
                &format!("/users/{user_id}"),
                &moderator,
                json!({ "email": "attacker@example.com" }),
            )
            .await,
            "e-mail change",
        ),
    ] {
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{what}");
        assert_eq!(
            response.code(),
            "INSUFFICIENT_ROLE",
            "{what}: {}",
            response.body
        );
    }
    // Content moderation stays with them.
    let renamed = app
        .patch(
            &format!("/users/{user_id}"),
            &moderator,
            json!({ "username": "customer.two" }),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    let banned = app
        .post(
            &format!("/users/{user_id}/ban"),
            &moderator,
            json!({ "duration_hours": 1 }),
        )
        .await;
    assert_eq!(banned.status, StatusCode::OK, "{}", banned.body);
    assert_eq!(
        app.get("/admin/audit-logs", &moderator).await.status,
        StatusCode::FORBIDDEN
    );

    // An admin's password reset and e-mail change reach the owner.
    app.delete(&format!("/users/{user_id}/ban"), &moderator)
        .await;
    let reset = app
        .post(
            &format!("/users/{user_id}/password-reset"),
            &admin,
            json!({ "new_password": "Temp#Pass2026" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT, "{}", reset.body);
    let (_, notice, _) = app
        .last_email("security_notice", Some("customer.one@example.com"))
        .await
        .expect("reset notice");
    assert_eq!(notice["kind"], "password_reset_by_staff");
    let moved = app
        .patch(
            &format!("/users/{user_id}"),
            &admin,
            json!({ "email": "customer.new@example.com" }),
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);
    let (_, notice, _) = app
        .last_email("security_notice", Some("customer.one@example.com"))
        .await
        .expect("e-mail change notice");
    assert_eq!(notice["kind"], "email_changed_by_staff");

    // Announcements: mass e-mail and publication are admin-only, and
    // buttons only link inside the app.
    let emailing = app
        .post(
            "/admin/announcements",
            &moderator,
            json!({ "title": "Hello there", "body": "x", "send_email": true }),
        )
        .await;
    assert_eq!(emailing.code(), "INSUFFICIENT_ROLE", "{}", emailing.body);
    let draft = app
        .post(
            "/admin/announcements",
            &moderator,
            json!({ "title": "Hello there", "body": "x", "show_banner": true }),
        )
        .await;
    assert_eq!(draft.status, StatusCode::CREATED, "{}", draft.body);
    let id = draft.body["id"].as_str().unwrap();
    let publish = app
        .post(
            &format!("/admin/announcements/{id}/publish"),
            &moderator,
            json!({}),
        )
        .await;
    assert_eq!(publish.code(), "INSUFFICIENT_ROLE");
    let phishing = app
        .post(
            "/admin/announcements",
            &admin,
            json!({ "title": "Billing", "body": "x", "show_banner": true,
                    "cta_label": "Pay", "cta_url": "https://setlyst-billing.example/login" }),
        )
        .await;
    assert_eq!(
        phishing.status,
        StatusCode::BAD_REQUEST,
        "{}",
        phishing.body
    );
}

#[tokio::test]
async fn staff_email_changes_never_strip_the_owners_second_factor() {
    let app = app!();
    let (_, admin) = app.user("takeover.admin", Role::Admin).await;
    let (victim_id, victim) = app
        .registered_user("victim.mod", "victim.mod@example.com")
        .await;
    let verified = app.verify_email(&victim, "victim.mod@example.com").await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    let setup = app
        .post(
            "/users/me/2fa/setup",
            &victim,
            json!({ "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(setup.status, StatusCode::OK, "{}", setup.body);
    let secret =
        setlyst_api::utils::totp::base32_decode(setup.body["secret"].as_str().unwrap()).unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    let enabled = app
        .post(
            "/users/me/2fa/enable",
            &victim,
            json!({ "code": setlyst_api::utils::totp::code_at(&secret, now) }),
        )
        .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
    app.state
        .security_repo
        .link_identity(
            victim_id,
            "google",
            "g-victim",
            Some("victim.mod@example.com"),
        )
        .await
        .unwrap();

    // A rogue admin points the address at a mailbox they control...
    let changed = app
        .patch(
            &format!("/users/{victim_id}"),
            &admin,
            json!({ "email": "attacker@example.com" }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    // ...and recovers the password through it. The address is proven for
    // the first time *since the change*, which used to count as the first
    // proof ever and wipe the second factor and the linked sign-ins.
    app.post_public(
        "/auth/password/forgot",
        json!({ "identifier": "victim.mod" }),
    )
    .await;
    let code = app
        .last_code("password_reset_code", "attacker@example.com")
        .await;
    let reset = app
        .post_public(
            "/auth/password/reset",
            json!({ "identifier": "victim.mod", "code": code, "new_password": "N3w!Password" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);

    let (two_factor, identities): (bool, i64) = sqlx::query_as(
        "SELECT totp_enabled_at IS NOT NULL,
                (SELECT COUNT(*) FROM oauth_identities WHERE user_id = $1)
         FROM users WHERE id = $1",
    )
    .bind(victim_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(
        two_factor,
        "the second factor must survive a staff e-mail change"
    );
    assert_eq!(identities, 1);
    // The new password alone does not sign in.
    let login = app.login_response("victim.mod", "N3w!Password").await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    assert_eq!(login.body["two_factor_required"], true, "{}", login.body);
    assert!(login.body["token"].is_null());
}

#[tokio::test]
async fn running_orders_with_lyrics_are_rate_limited_per_account() {
    let app = app!();
    let (_, user) = app.user("heavy.reader", Role::User).await;
    let setlist = app.setlist(&user, "Big", None).await;
    let path = format!("/setlists/{setlist}/items");
    for _ in 0..300 {
        assert_eq!(app.get(&path, &user).await.status, StatusCode::OK);
    }
    let limited = app.get(&path, &user).await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.body
    );
    assert_eq!(limited.code(), "TOO_MANY_ATTEMPTS");
    assert!(
        limited.body["meta"]["retry_after_seconds"]
            .as_i64()
            .unwrap()
            > 0
    );
    // Per account: nobody else is slowed down.
    let (_, other) = app.user("light.reader", Role::User).await;
    let own = app.setlist(&other, "Small", None).await;
    assert_eq!(
        app.get(&format!("/setlists/{own}/items"), &other)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn public_links_are_only_shown_to_members_who_may_manage_the_item() {
    let app = app!();
    let (_, owner) = app.user("link.owner", Role::User).await;
    let (_, member) = app.user("link.member", Role::User).await;
    let band = app.band(&owner, "Link Band").await;
    app.join_band(&owner, &member, &band, None).await;
    let setlist = app.setlist(&owner, "Shared", Some(&band)).await;
    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &owner, json!({}))
        .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.body);
    let token = shared.body["share_token"].as_str().unwrap().to_string();
    let gig = app
        .post(
            "/gigs",
            &owner,
            json!({ "venue": "Clube", "scheduled_at": "2030-03-01T22:00:00", "band_id": band,
                    "setlist_id": setlist }),
        )
        .await;
    assert_eq!(gig.status, StatusCode::CREATED, "{}", gig.body);
    let gig_id = gig.body["id"].as_str().unwrap().to_string();
    let gig_shared = app
        .post(&format!("/gigs/{gig_id}/share"), &owner, json!({}))
        .await;
    assert_eq!(gig_shared.status, StatusCode::OK, "{}", gig_shared.body);
    let gig_token = gig_shared.body["share_token"].as_str().unwrap().to_string();

    // The owner (who may manage both) sees the links; a plain member
    // (who may not, by default) does not: they could hand them out and
    // keep using them after leaving.
    assert_eq!(
        app.get(&format!("/setlists/{setlist}"), &owner).await.body["share_token"],
        token.as_str()
    );
    assert_eq!(
        app.get(&format!("/gigs/{gig_id}"), &owner).await.body["share_token"],
        gig_token.as_str()
    );
    let seen = app.get(&format!("/setlists/{setlist}"), &member).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert!(seen.body["share_token"].is_null(), "{}", seen.body);
    let seen_gig = app.get(&format!("/gigs/{gig_id}"), &member).await;
    assert_eq!(seen_gig.status, StatusCode::OK, "{}", seen_gig.body);
    assert!(seen_gig.body["share_token"].is_null(), "{}", seen_gig.body);

    // Once members may manage setlists, they see them.
    let updated = app
        .put(
            &format!("/bands/{band}/permissions"),
            &owner,
            json!({ "permissions": [
                { "role": "member", "permission": "manage_setlists", "allowed": true }
            ] }),
        )
        .await;
    assert!(updated.status.is_success(), "{}", updated.body);
    assert_eq!(
        app.get(&format!("/setlists/{setlist}"), &member).await.body["share_token"],
        token.as_str()
    );
    assert_eq!(
        app.get(&format!("/gigs/{gig_id}"), &member).await.body["share_token"],
        gig_token.as_str()
    );
}
