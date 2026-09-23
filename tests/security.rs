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
    let (_, moderator) = app.user("pager", Role::Moderator).await;
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
        assert_eq!(response.body["meta"]["current_page"], 100_000, "{path}");
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
        vec!["status", "version"]
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
    assert_eq!(legal.body["version"], "2026-09-23");
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

    // With a second admin, demotions work until one admin is left.
    let (second_id, second) = app.user("second.admin", Role::Admin).await;
    let demoted = app
        .patch(
            &format!("/users/{first_id}"),
            &second,
            json!({ "role": "user" }),
        )
        .await;
    assert_eq!(demoted.status, StatusCode::OK, "{}", demoted.body);
    let (_, third) = {
        let id = app
            .create_user("third.admin", STRONG_PASSWORD, Role::Admin)
            .await;
        (id, app.login("third.admin", STRONG_PASSWORD).await)
    };
    // Two concurrent demotions of the two remaining admins: at most one
    // wins, so an admin always remains.
    let third_id = app
        .state
        .user_repo
        .find_by_identifier("third.admin")
        .await
        .unwrap()
        .unwrap()
        .id;
    let second_path = format!("/users/{second_id}");
    let third_path = format!("/users/{third_id}");
    let (a, b) = tokio::join!(
        app.patch(&second_path, &third, json!({ "role": "moderator" })),
        app.patch(&third_path, &second, json!({ "role": "moderator" })),
    );
    let admins = app.state.user_repo.count_active_admins().await.unwrap();
    assert!(admins >= 1, "{} / {}", a.body, b.body);
    assert!(
        [a.status, b.status].contains(&StatusCode::CONFLICT)
            || [a.status, b.status].contains(&StatusCode::UNAUTHORIZED)
            || [a.status, b.status].contains(&StatusCode::FORBIDDEN),
        "{} / {}",
        a.body,
        b.body
    );
}

#[tokio::test]
async fn failed_sign_ins_of_unknown_accounts_are_audited() {
    let app = app!();
    let response = app.login_response("ghost.account", STRONG_PASSWORD).await;
    assert_eq!(response.code(), "INVALID_CREDENTIALS");
    let mut found = 0i64;
    for _ in 0..50 {
        found = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_logs WHERE action = 'user.login_failed' AND target_label = 'ghost.account'",
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
