//! Announcements (audience, channels, receipts, fan-out) and release notes.

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::json;
use setlyst_api::{jobs, models::user::Role};

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn staff_create_validate_and_target_announcements() {
    let app = app!();
    let (_, moderator) = app.user("ann.mod", Role::Moderator).await;
    let (_, user) = app.user("ann.user", Role::User).await;

    let forbidden = app
        .post(
            "/admin/announcements",
            &user,
            json!({ "title": "Hello", "body": "x", "show_banner": true }),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    for (body, why) in [
        (
            json!({ "title": "Hi", "body": "x", "show_banner": true }),
            "short title",
        ),
        (
            json!({ "title": "Hello", "body": "x", "send_notification": false }),
            "no channel",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "requires_acknowledgement": true }),
            "ack without modal",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "cta_url": "/x" }),
            "url without label",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "cta_label": "Go", "cta_url": "//evil.example" }),
            "protocol-relative",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "audience_roles": ["root"] }),
            "role",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "audience_plans": ["gold"] }),
            "plan",
        ),
        (
            json!({ "title": "Hello", "body": "x", "show_banner": true, "audience_locales": ["fr"] }),
            "locale",
        ),
    ] {
        let response = app.post("/admin/announcements", &moderator, body).await;
        assert_eq!(
            response.code(),
            "VALIDATION_ERROR",
            "{why}: {}",
            response.body
        );
    }

    // Audience preview: roles and locales.
    app.patch(
        "/users/me/preferences",
        &user,
        json!({ "language": "pt-BR" }),
    )
    .await;
    let preview = |body: serde_json::Value| {
        app.post("/admin/announcements/preview-audience", &moderator, body)
    };
    assert_eq!(preview(json!({})).await.body["count"], 2);
    assert_eq!(
        preview(json!({ "audience_roles": ["moderator"] }))
            .await
            .body["count"],
        1
    );
    assert_eq!(
        preview(json!({ "audience_locales": ["pt-BR"] })).await.body["count"],
        1
    );
    assert_eq!(
        preview(json!({ "audience_plans": ["none"] })).await.body["count"],
        2
    );
    assert_eq!(
        preview(json!({ "audience_plans": ["trial"] })).await.body["count"],
        0
    );

    let created = app
        .post(
            "/admin/announcements",
            &moderator,
            json!({ "title": "Manutenção programada", "body": "Voltamos logo.", "level": "warning",
                    "show_banner": true, "audience_locales": ["pt-BR"], "cta_label": "Ver", "cta_url": "/dashboard" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["status"], "draft");
    assert_eq!(created.body["stats"]["targeted"], 1);
    let id = created.body["id"].as_str().unwrap().to_string();

    let listed = app
        .get("/admin/announcements?status=draft", &moderator)
        .await;
    assert_eq!(listed.body["meta"]["total_items"], 1);
    let edited = app
        .patch(
            &format!("/admin/announcements/{id}"),
            &moderator,
            json!({ "cta_label": null, "cta_url": null }),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    assert!(edited.body["cta_url"].is_null());

    // Deleting a draft removes it.
    assert_eq!(
        app.delete(&format!("/admin/announcements/{id}"), &moderator)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get(&format!("/admin/announcements/{id}"), &moderator)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn published_announcements_reach_their_audience_once() {
    let app = app!();
    let (_, admin) = app.user("pub.admin", Role::Admin).await;
    let (_, reader) = app
        .registered_user("reader.one", "reader.one@example.com")
        .await;
    app.verify_email(&reader, "reader.one@example.com").await;
    let (_, muted) = app
        .registered_user("reader.two", "reader.two@example.com")
        .await;
    app.put(
        "/users/me/communication",
        &muted,
        json!({ "categories": { "announcements": { "in_app": false, "email": true } } }),
    )
    .await;
    let (_, moderator) = app.user("pub.mod", Role::Moderator).await;

    let created = app
        .post(
            "/admin/announcements",
            &admin,
            json!({ "title": "Nova versão", "body": "Chegou a versão 0.12.\n\nConfira.",
                    "show_modal": true, "show_banner": true, "send_email": true,
                    "requires_acknowledgement": true, "dismissible": false,
                    "audience_roles": ["user"] }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();

    // Nothing is visible before publication.
    assert_eq!(
        app.get("/announcements", &reader).await.body["meta"]["total_items"],
        0
    );

    let published = app
        .post(
            &format!("/admin/announcements/{id}/publish"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(published.status, StatusCode::OK, "{}", published.body);
    assert_eq!(published.body["status"], "active");
    assert!(published.body["delivered_at"].is_string());

    // In-app notifications respect the preference; e-mail goes to the
    // verified reader only.
    let notifications: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE type = 'announcement'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(notifications, 1);
    let emails: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM email_outbox WHERE template = 'announcement'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(emails, 1);
    // The job finds nothing left to deliver: fan-out happens once.
    assert_eq!(
        jobs::announcements::deliver_due(&app.state).await.unwrap(),
        0
    );

    // Only targeted users see it.
    assert_eq!(
        app.get("/announcements", &moderator).await.body["meta"]["total_items"],
        0
    );
    let active = app.get("/announcements/active", &reader).await;
    assert_eq!(active.body["modal"].as_array().unwrap().len(), 1);
    assert_eq!(active.body["banner"].as_array().unwrap().len(), 1);
    assert!(active.body["modal"][0]["receipt"]["seen_at"].is_null());

    let seen = app
        .post(&format!("/announcements/{id}/seen"), &reader, json!({}))
        .await;
    assert!(seen.body["seen_at"].is_string());
    let dismiss = app
        .post(&format!("/announcements/{id}/dismiss"), &reader, json!({}))
        .await;
    assert_eq!(dismiss.status, StatusCode::CONFLICT);
    assert_eq!(dismiss.code(), "NOT_DISMISSIBLE");
    let ack = app
        .post(
            &format!("/announcements/{id}/acknowledge"),
            &reader,
            json!({}),
        )
        .await;
    assert!(ack.body["acknowledged_at"].is_string());
    let active = app.get("/announcements/active", &reader).await;
    assert_eq!(active.body["modal"].as_array().unwrap().len(), 0);
    assert_eq!(active.body["banner"].as_array().unwrap().len(), 1);

    // Stats and edit rules while active.
    let stats = app.get(&format!("/admin/announcements/{id}"), &admin).await;
    assert_eq!(stats.body["stats"]["targeted"], 2);
    assert_eq!(stats.body["stats"]["seen"], 1);
    assert_eq!(stats.body["stats"]["acknowledged"], 1);
    let locked = app
        .patch(
            &format!("/admin/announcements/{id}"),
            &admin,
            json!({ "audience_roles": null }),
        )
        .await;
    assert_eq!(locked.code(), "ANNOUNCEMENT_LOCKED");
    let text = app
        .patch(
            &format!("/admin/announcements/{id}"),
            &admin,
            json!({ "title": "Nova versão 0.12" }),
        )
        .await;
    assert_eq!(text.status, StatusCode::OK, "{}", text.body);

    // Unknown ids and other users' announcements are not found.
    let missing = app
        .post(
            &format!("/announcements/{}/seen", uuid::Uuid::new_v4()),
            &reader,
            json!({}),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.post(&format!("/announcements/{id}/seen"), &moderator, json!({}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Deleting a published one archives it; it disappears for users.
    app.delete(&format!("/admin/announcements/{id}"), &admin)
        .await;
    assert_eq!(
        app.get("/announcements", &reader).await.body["meta"]["total_items"],
        0
    );
    assert_eq!(
        app.get("/admin/announcements?status=archived", &admin)
            .await
            .body["meta"]["total_items"],
        1
    );
}

#[tokio::test]
async fn scheduled_announcements_wait_for_their_window() {
    let app = app!();
    let (_, admin) = app.user("sched.admin", Role::Admin).await;
    let (_, user) = app.user("sched.user", Role::User).await;
    let start = chrono::Utc::now().naive_utc() + chrono::Duration::hours(1);
    let created = app
        .post(
            "/admin/announcements",
            &admin,
            json!({ "title": "Later", "body": "Soon.", "show_banner": true, "starts_at": start }),
        )
        .await;
    let id = created.body["id"].as_str().unwrap().to_string();
    let published = app
        .post(
            &format!("/admin/announcements/{id}/publish"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(published.body["status"], "scheduled");
    assert!(published.body["delivered_at"].is_null());
    assert_eq!(
        app.get("/announcements/active", &user).await.body["banner"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // Once the window opens, the job delivers it.
    sqlx::query("UPDATE announcements SET starts_at = NOW() AT TIME ZONE 'utc' - INTERVAL '1 minute' WHERE id = $1::uuid")
        .bind(&id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        jobs::announcements::deliver_due(&app.state).await.unwrap(),
        1
    );
    assert_eq!(
        app.get("/notifications/unread-count", &user).await.body["unread_count"],
        1
    );
    let dismissed = app
        .post(&format!("/announcements/{id}/dismiss"), &user, json!({}))
        .await;
    assert_eq!(dismissed.status, StatusCode::OK);
    assert_eq!(
        app.get("/announcements/active", &user).await.body["banner"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // History keeps it.
    assert_eq!(
        app.get("/announcements", &user).await.body["meta"]["total_items"],
        1
    );
}

#[tokio::test]
async fn release_notes_drafts_publication_and_edits() {
    let app = app!();
    let (_, admin) = app.user("notes.admin", Role::Admin).await;
    let (_, moderator) = app.user("notes.mod", Role::Moderator).await;
    let (_, user) = app
        .registered_user("notes.user", "notes.user@example.com")
        .await;
    app.verify_email(&user, "notes.user@example.com").await;
    app.put(
        "/users/me/communication",
        &user,
        json!({ "categories": { "product_updates": { "email": true, "in_app": true } } }),
    )
    .await;

    let public = app
        .request(Method::GET, "/public/release-notes", None, None)
        .await;
    let seeded = public.body.as_array().unwrap().len();
    assert_eq!(seeded, 3, "v0.10, v0.11 and v0.12 are seeded");
    assert_eq!(public.body[0]["version"], "0.12.0");
    for note in public.body.as_array().unwrap() {
        assert_eq!(note["is_edited"], false, "seeded notes are not edited");
        assert_eq!(note["updated_at"], note["published_at"]);
    }

    let body = json!({
        "version": "0.13.0",
        "title": { "en": "Tours and more", "pt-BR": "Turnês e mais" },
        "items": [{ "kind": "new", "text": { "en": "Plan tours.", "pt-BR": "Planeje turnês." } }],
        "released_on": "2026-09-30"
    });
    let by_moderator = app
        .post("/admin/release-notes", &moderator, body.clone())
        .await;
    assert_eq!(by_moderator.status, StatusCode::FORBIDDEN);
    let invalid = app
        .post(
            "/admin/release-notes",
            &admin,
            json!({ "version": "v1", "title": {}, "items": [], "released_on": "2026-09-30" }),
        )
        .await;
    assert_eq!(invalid.code(), "VALIDATION_ERROR");

    let created = app.post("/admin/release-notes", &admin, body.clone()).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    assert!(created.body["published_at"].is_null());
    assert_eq!(
        app.post("/admin/release-notes", &admin, body).await.code(),
        "ALREADY_EXISTS"
    );

    // Drafts are staff-only.
    let public = app
        .request(Method::GET, "/public/release-notes", None, None)
        .await;
    assert_eq!(public.body.as_array().unwrap().len(), seeded);
    assert_eq!(
        app.get("/admin/release-notes", &moderator)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        seeded + 1
    );

    let published = app
        .post(
            &format!("/admin/release-notes/{id}/publish"),
            &admin,
            json!({ "notify": true }),
        )
        .await;
    assert_eq!(published.status, StatusCode::OK, "{}", published.body);
    assert_eq!(published.body["is_edited"], false);
    let public = app
        .request(Method::GET, "/public/release-notes", None, None)
        .await;
    assert_eq!(public.body[0]["version"], "0.13.0");
    let notified: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE type = 'release_published'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(notified, 3, "every active account");
    let (to, payload, _) = app
        .last_email("release_notes", None)
        .await
        .expect("release e-mail");
    assert_eq!(to, "notes.user@example.com");
    assert_eq!(payload["version"], "0.13.0");

    // Editing a published note marks it edited (once past the grace minute).
    sqlx::query("UPDATE release_notes SET published_at = published_at - INTERVAL '10 minutes' WHERE id = $1::uuid")
        .bind(&id)
        .execute(&app.pool)
        .await
        .unwrap();
    let edited = app
        .patch(
            &format!("/admin/release-notes/{id}"),
            &admin,
            json!({ "title": { "en": "Tours!", "pt-BR": "Turnês!" } }),
        )
        .await;
    assert_eq!(edited.body["is_edited"], true);
    assert_eq!(edited.body["updated_by_username"], "notes.admin");

    let unpublished = app
        .post(
            &format!("/admin/release-notes/{id}/unpublish"),
            &admin,
            json!({}),
        )
        .await;
    assert!(unpublished.body["published_at"].is_null());
    assert_eq!(
        app.delete(&format!("/admin/release-notes/{id}"), &admin)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}
