//! Platform operations: maintenance mode and sign-up switches, the
//! support desk, staff notes, status incidents, the e-mail console, the
//! console overview and search, CSV exports, bulk actions and sign-in
//! activity.

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

fn platform(mode: &str, registrations_open: bool, blocked: &[&str]) -> serde_json::Value {
    json!({
        "maintenance": { "mode": mode, "message": "Back at 22:00" },
        "registrations_open": registrations_open,
        "blocked_email_domains": blocked,
    })
}

#[tokio::test]
async fn maintenance_modes_spare_staff_and_reads() {
    let app = app!();
    let (_, admin) = app.user("ops.admin", Role::Admin).await;
    let (_, moderator) = app.user("ops.mod", Role::Moderator).await;
    let (_, user) = app.user("ops.user", Role::User).await;

    // Only admins change the switches; staff read them.
    let refused = app
        .put(
            "/admin/settings/platform",
            &moderator,
            platform("full", true, &[]),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let current = app.get("/admin/settings/platform", &moderator).await;
    assert_eq!(current.status, StatusCode::OK);
    assert_eq!(current.body["maintenance"]["mode"], "off");
    assert_eq!(current.body["registrations_open"], true);

    // Read-only: reads work, writes answer MAINTENANCE_MODE (503).
    let saved = app
        .put(
            "/admin/settings/platform",
            &admin,
            platform("read_only", true, &[]),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert!(saved.body["maintenance"]["started_at"].is_string());
    let public = app
        .request(Method::GET, "/public/platform", None, None)
        .await;
    assert_eq!(public.body["maintenance"]["mode"], "read_only");
    assert_eq!(public.body["maintenance"]["message"], "Back at 22:00");

    assert_eq!(app.get("/songs", &user).await.status, StatusCode::OK);
    let write = app.post("/artists", &user, json!({ "name": "Nope" })).await;
    assert_eq!(write.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(write.code(), "MAINTENANCE_MODE");
    assert_eq!(write.body["meta"]["mode"], "read_only");
    assert!(write.headers.get("retry-after").is_some());
    // Staff keep working.
    let staff_write = app
        .post("/artists", &moderator, json!({ "name": "Staff" }))
        .await;
    assert_eq!(staff_write.status, StatusCode::CREATED);
    // Sign-in still works in read-only mode, sign-up doesn't.
    app.login("ops.user", STRONG_PASSWORD).await;
    let signup = app
        .register("ops.new", "ops.new@example.com", json!({}))
        .await;
    assert_eq!(signup.code(), "MAINTENANCE_MODE");

    // Full: nothing for regular accounts, sign-in included.
    app.put(
        "/admin/settings/platform",
        &admin,
        platform("full", true, &[]),
    )
    .await;
    let read = app.get("/songs", &user).await;
    assert_eq!(read.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(read.code(), "MAINTENANCE_MODE");
    let login = app.login_response("ops.user", STRONG_PASSWORD).await;
    assert_eq!(login.code(), "MAINTENANCE_MODE");
    app.login("ops.mod", STRONG_PASSWORD).await;
    assert_eq!(app.get("/songs", &moderator).await.status, StatusCode::OK);

    // Back to normal.
    let off = app
        .put(
            "/admin/settings/platform",
            &admin,
            platform("off", true, &[]),
        )
        .await;
    assert!(off.body["maintenance"]["started_at"].is_null());
    assert_eq!(app.get("/songs", &user).await.status, StatusCode::OK);

    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'settings.platform_updated'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audited, 3);
}

#[tokio::test]
async fn sign_ups_can_be_closed_and_domains_blocked() {
    let app = app!();
    let (_, admin) = app.user("reg.admin", Role::Admin).await;

    let invalid = app
        .put(
            "/admin/settings/platform",
            &admin,
            platform("off", true, &["not a domain"]),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

    let saved = app
        .put(
            "/admin/settings/platform",
            &admin,
            platform("off", true, &["@Mailinator.com", "tempmail.io"]),
        )
        .await;
    assert_eq!(
        saved.body["blocked_email_domains"],
        json!(["mailinator.com", "tempmail.io"])
    );

    let blocked = app
        .register("reg.spam", "x@eu.mailinator.com", json!({}))
        .await;
    assert_eq!(blocked.status, StatusCode::BAD_REQUEST);
    assert_eq!(blocked.code(), "EMAIL_DOMAIN_BLOCKED");
    let fine = app
        .register("reg.fine", "fine@example.com", json!({}))
        .await;
    assert_eq!(fine.status, StatusCode::CREATED, "{}", fine.body);

    // An existing account can't move to a blocked domain either.
    let (_, user) = app.user("reg.user", Role::User).await;
    let change = app
        .post(
            "/users/me/email/change",
            &user,
            json!({ "new_email": "me@tempmail.io", "password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(change.code(), "EMAIL_DOMAIN_BLOCKED");

    app.put(
        "/admin/settings/platform",
        &admin,
        platform("off", false, &[]),
    )
    .await;
    let closed = app
        .register("reg.closed", "closed@example.com", json!({}))
        .await;
    assert_eq!(closed.status, StatusCode::FORBIDDEN);
    assert_eq!(closed.code(), "REGISTRATION_CLOSED");
    let public = app
        .request(Method::GET, "/public/platform", None, None)
        .await;
    assert_eq!(public.body["registrations_open"], false);
    assert!(public.body.get("blocked_email_domains").is_none());
}

#[tokio::test]
async fn support_requests_flow_between_requester_and_staff() {
    let app = app!();
    let (user_id, user) = app.user("help.user", Role::User).await;
    let (_, other) = app.user("help.other", Role::User).await;
    let (moderator_id, moderator) = app.user("help.mod", Role::Moderator).await;

    let invalid = app
        .post(
            "/support/tickets",
            &user,
            json!({ "subject": "x", "category": "bug", "body": "" }),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

    let created = app
        .post(
            "/support/tickets",
            &user,
            json!({
                "subject": "I can't export my setlist",
                "category": "billing",
                "body": "The PDF button does nothing.",
                "context": { "page": "/dashboard/setlists/1" },
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let ticket_id = created.body["ticket"]["id"].as_str().unwrap().to_string();
    let number = created.body["ticket"]["number"].as_i64().unwrap();
    assert!(number >= 1001);
    assert_eq!(created.body["ticket"]["status"], "open");
    assert_eq!(created.body["messages"].as_array().unwrap().len(), 1);

    // Nobody else sees it; staff do, in the inbox.
    let foreign = app
        .get(&format!("/support/tickets/{ticket_id}"), &other)
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get("/admin/support/tickets", &user).await.status,
        StatusCode::FORBIDDEN
    );
    let inbox = app.get("/admin/support/tickets", &moderator).await;
    assert_eq!(inbox.body["meta"]["total_items"], 1);
    let row = &inbox.body["data"][0];
    assert_eq!(row["priority"], "high", "billing starts high");
    assert_eq!(row["username"], "help.user");
    assert_eq!(row["last_from_requester"], true);
    let by_number = app
        .get(&format!("/admin/support/tickets?q=%23{number}"), &moderator)
        .await;
    assert_eq!(by_number.body["meta"]["total_items"], 1);

    // An internal note stays internal and changes nothing for the
    // requester.
    let note = app
        .post(
            &format!("/admin/support/tickets/{ticket_id}/messages"),
            &moderator,
            json!({ "body": "Probably the popup blocker.", "internal": true }),
        )
        .await;
    assert_eq!(note.status, StatusCode::CREATED, "{}", note.body);
    assert_eq!(note.body["ticket"]["status"], "open");
    assert!(note.body["ticket"]["assignee_id"].is_null());

    // A public reply waits for the requester, assigns the ticket and
    // notifies them.
    let reply = app
        .post(
            &format!("/admin/support/tickets/{ticket_id}/messages"),
            &moderator,
            json!({ "body": "Could you allow pop-ups and try again?" }),
        )
        .await;
    assert_eq!(reply.body["ticket"]["status"], "pending");
    assert_eq!(
        reply.body["ticket"]["assignee_id"],
        moderator_id.to_string()
    );
    assert!(reply.body["ticket"]["first_response_at"].is_string());
    assert_eq!(reply.body["messages"].as_array().unwrap().len(), 3);
    let notified: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND type = 'support_reply'",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(notified, 1);

    let summary = app.get("/support/summary", &user).await;
    assert_eq!(summary.body["unread"], 1);
    let detail = app
        .get(&format!("/support/tickets/{ticket_id}"), &user)
        .await;
    let messages = detail.body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "the internal note is hidden");
    assert!(messages.iter().all(|m| m["internal"] == false));
    assert_eq!(
        app.get("/support/summary", &user).await.body["unread"],
        0,
        "opening it marks it read"
    );

    // Rating needs a resolved ticket.
    let early = app
        .post(
            &format!("/support/tickets/{ticket_id}/rating"),
            &user,
            json!({ "rating": 5 }),
        )
        .await;
    assert_eq!(early.code(), "TICKET_NOT_RATEABLE");

    // The requester's reply opens it again.
    let answer = app
        .post(
            &format!("/support/tickets/{ticket_id}/messages"),
            &user,
            json!({ "body": "That was it, thanks!" }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::CREATED);
    let resolved = app
        .patch(
            &format!("/admin/support/tickets/{ticket_id}"),
            &moderator,
            json!({ "status": "resolved", "priority": "low" }),
        )
        .await;
    assert_eq!(resolved.body["ticket"]["status"], "resolved");
    assert!(resolved.body["ticket"]["resolved_at"].is_string());
    let bad_assignee = app
        .patch(
            &format!("/admin/support/tickets/{ticket_id}"),
            &moderator,
            json!({ "assignee_id": user_id }),
        )
        .await;
    assert_eq!(bad_assignee.status, StatusCode::BAD_REQUEST);

    let rated = app
        .post(
            &format!("/support/tickets/{ticket_id}/rating"),
            &user,
            json!({ "rating": 5, "comment": "Fast!" }),
        )
        .await;
    assert_eq!(rated.status, StatusCode::OK, "{}", rated.body);
    assert_eq!(rated.body["rating"], 5);
    let twice = app
        .post(
            &format!("/support/tickets/{ticket_id}/rating"),
            &user,
            json!({ "rating": 1 }),
        )
        .await;
    assert_eq!(twice.code(), "TICKET_NOT_RATEABLE");

    let closed = app
        .post(
            &format!("/support/tickets/{ticket_id}/close"),
            &user,
            json!({}),
        )
        .await;
    assert_eq!(closed.body["status"], "closed");
    let late = app
        .post(
            &format!("/support/tickets/{ticket_id}/messages"),
            &user,
            json!({ "body": "One more thing" }),
        )
        .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(late.code(), "TICKET_CLOSED");

    let stats = app.get("/admin/support/summary", &moderator).await;
    assert_eq!(stats.body["closed"], 1);
    assert_eq!(stats.body["average_rating"], 5.0);
    assert!(stats.body["median_first_response_minutes"].is_number());

    // The personal data export carries the public conversation.
    let export = app.get("/users/me/data-export", &user).await;
    let tickets = export.body["support_tickets"].as_array().unwrap();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0]["messages"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn support_requests_are_limited() {
    let app = app!();
    let (_, user) = app.user("limit.user", Role::User).await;
    for i in 0..5 {
        let created = app
            .post(
                "/support/tickets",
                &user,
                json!({ "subject": format!("Question {i}"), "category": "other", "body": "?" }),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED);
    }
    let sixth = app
        .post(
            "/support/tickets",
            &user,
            json!({ "subject": "Question 6", "category": "other", "body": "?" }),
        )
        .await;
    assert_eq!(sixth.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(sixth.code(), "SUPPORT_TICKET_LIMIT");
    assert_eq!(sixth.body["meta"]["reason"], "open");
    let mine = app.get("/support/tickets", &user).await;
    assert_eq!(mine.body["meta"]["total_items"], 5);
}

#[tokio::test]
async fn staff_notes_are_internal_and_owned_by_their_author() {
    let app = app!();
    let (user_id, user) = app.user("note.user", Role::User).await;
    let (moderator_id, moderator) = app.user("note.mod", Role::Moderator).await;
    let (_, other_mod) = app.user("note.mod2", Role::Moderator).await;
    let (_, admin) = app.user("note.admin", Role::Admin).await;

    let path = format!("/admin/users/{user_id}/notes");
    assert_eq!(app.get(&path, &user).await.status, StatusCode::FORBIDDEN);
    let own = app
        .post(
            &format!("/admin/users/{moderator_id}/notes"),
            &moderator,
            json!({ "body": "me" }),
        )
        .await;
    assert_eq!(own.code(), "CANNOT_TARGET_SELF");

    let created = app
        .post(
            &path,
            &moderator,
            json!({ "body": "Asked for a refund by e-mail." }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let note_id = created.body["id"].as_str().unwrap().to_string();
    assert_eq!(created.body["author_username"], "note.mod");

    let pinned = app
        .post(
            &path,
            &other_mod,
            json!({ "body": "Watch the avatar.", "pinned": true }),
        )
        .await;
    let listed = app.get(&path, &admin).await;
    let notes = listed.body.as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["id"], pinned.body["id"], "pinned first");

    // Only the author (or an admin) edits or deletes.
    let foreign_edit = app
        .patch(
            &format!("{path}/{note_id}"),
            &other_mod,
            json!({ "body": "changed" }),
        )
        .await;
    assert_eq!(foreign_edit.code(), "INSUFFICIENT_ROLE");
    let edited = app
        .patch(
            &format!("{path}/{note_id}"),
            &moderator,
            json!({ "body": "Refund done.", "pinned": true }),
        )
        .await;
    assert_eq!(edited.body["body"], "Refund done.");
    assert_eq!(
        app.delete(&format!("{path}/{note_id}"), &admin)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get(&path, &moderator)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The account sees what was noted, not who wrote it.
    let export = app.get("/users/me/data-export", &user).await;
    let exported = export.body["staff_notes"].as_array().unwrap();
    assert_eq!(exported.len(), 1);
    assert!(exported[0].get("author_username").is_none());
}

#[tokio::test]
async fn incidents_reach_the_public_status_page() {
    let app = app!();
    let (_, moderator) = app.user("inc.mod", Role::Moderator).await;
    let (_, admin) = app.user("inc.admin", Role::Admin).await;

    let no_window = app
        .post(
            "/admin/incidents",
            &moderator,
            json!({
                "kind": "maintenance", "title": "Database upgrade", "impact": "minor",
                "message": "We are upgrading the database."
            }),
        )
        .await;
    assert_eq!(no_window.status, StatusCode::BAD_REQUEST);
    let bad_component = app
        .post(
            "/admin/incidents",
            &moderator,
            json!({
                "kind": "incident", "title": "Slow", "impact": "minor",
                "components": ["kitchen"], "message": "Slow."
            }),
        )
        .await;
    assert_eq!(bad_component.status, StatusCode::BAD_REQUEST);

    let created = app
        .post(
            "/admin/incidents",
            &moderator,
            json!({
                "kind": "incident", "title": "E-mails delayed", "impact": "major",
                "components": ["email", "email"], "message": "Codes take a few minutes."
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    assert_eq!(created.body["status"], "investigating");
    assert_eq!(created.body["components"], json!(["email"]));
    assert!(created.body["started_at"].is_string());

    let public = app
        .request(Method::GET, "/public/incidents", None, None)
        .await;
    assert_eq!(public.body["active"].as_array().unwrap().len(), 1);
    assert_eq!(
        public.body["active"][0]["updates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let resolved = app
        .post(
            &format!("/admin/incidents/{id}/updates"),
            &moderator,
            json!({ "status": "resolved", "body": "Delivery is back to normal." }),
        )
        .await;
    assert_eq!(resolved.status, StatusCode::CREATED);
    assert_eq!(resolved.body["status"], "resolved");
    assert_eq!(resolved.body["updates"].as_array().unwrap().len(), 2);
    assert_eq!(resolved.body["updates"][0]["status"], "resolved");

    let public = app
        .request(Method::GET, "/public/incidents", None, None)
        .await;
    assert!(public.body["active"].as_array().unwrap().is_empty());
    assert_eq!(public.body["recent"].as_array().unwrap().len(), 1);

    assert_eq!(
        app.delete(&format!("/admin/incidents/{id}"), &moderator)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.delete(&format!("/admin/incidents/{id}"), &admin)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn the_email_console_lists_and_retries_without_exposing_variables() {
    let app = app!();
    let (_, admin) = app.user("mail.admin", Role::Admin).await;
    let (_, moderator) = app.user("mail.mod", Role::Moderator).await;
    // A sign-up queues a verification code.
    app.registered_user("mail.new", "mail.new@example.com")
        .await;
    // And a failed announcement copy, retryable.
    let failed_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO email_outbox (id, to_email, to_canonical, template, locale, payload, status,
                                   attempts, last_error, scheduled_at, created_at, priority)
         VALUES ($1, 'x@example.com', 'x@example.com', 'announcement', 'en',
                 '{\"title\": \"t\", \"body\": \"b\", \"level\": \"info\"}', 'failed', 5,
                 'mailbox full', NOW(), NOW(), 9)",
    )
    .bind(failed_id)
    .execute(&app.pool)
    .await
    .unwrap();

    let listed = app.get("/admin/emails", &moderator).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert!(listed.body["meta"]["total_items"].as_i64().unwrap() >= 2);
    assert!(
        listed.body.to_string().find("\"code\"").is_none(),
        "template variables are never exposed"
    );
    let codes = app
        .get("/admin/emails?template=email_verification_code", &moderator)
        .await;
    assert_eq!(codes.body["data"][0]["retryable"], false);
    let failed = app.get("/admin/emails?status=failed", &moderator).await;
    assert_eq!(failed.body["meta"]["total_items"], 1);
    assert_eq!(failed.body["data"][0]["retryable"], true);
    assert_eq!(failed.body["data"][0]["last_error"], "mailbox full");

    let summary = app.get("/admin/emails/summary", &moderator).await;
    assert_eq!(summary.body["last_24h"]["failed"], 1);
    assert_eq!(summary.body["smtp_configured"], false);

    assert_eq!(
        app.post(
            &format!("/admin/emails/{failed_id}/retry"),
            &moderator,
            json!({})
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let retried = app
        .post(
            &format!("/admin/emails/{failed_id}/retry"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(retried.status, StatusCode::OK, "{}", retried.body);
    assert_eq!(retried.body["status"], "pending");
    assert_eq!(retried.body["attempts"], 0);
    let again = app
        .post(
            &format!("/admin/emails/{failed_id}/retry"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(again.code(), "EMAIL_NOT_RETRYABLE");
    let canceled = app
        .post(
            &format!("/admin/emails/{failed_id}/cancel"),
            &admin,
            json!({}),
        )
        .await;
    assert_eq!(canceled.body["status"], "skipped");

    // No SMTP server in tests: the test message is refused.
    let test = app.post("/admin/emails/test", &admin, json!({})).await;
    assert_eq!(test.code(), "EMAIL_NOT_CONFIGURED");
}

#[tokio::test]
async fn overview_search_exports_bulk_actions_and_sign_ins() {
    let app = app!();
    let (_, admin) = app.user("con.admin", Role::Admin).await;
    let (_, moderator) = app.user("con.mod", Role::Moderator).await;
    let (target_a, user_a) = app.user("con.alpha", Role::User).await;
    let (target_b, _) = app.user("con.beta", Role::User).await;
    let (other_mod, _) = app.user("con.mod2", Role::Moderator).await;
    let artist = app.artist(&user_a, "Alpha Band").await;
    app.song(&user_a, &artist, "Alpha Song").await;

    let overview = app.get("/admin/overview", &moderator).await;
    assert_eq!(overview.status, StatusCode::OK, "{}", overview.body);
    assert_eq!(overview.body["users"]["total"], 5);
    assert_eq!(overview.body["users"]["new_today"], 5);
    assert_eq!(overview.body["signups"].as_array().unwrap().len(), 30);
    assert_eq!(overview.body["maintenance_mode"], "off");
    assert_eq!(
        overview.body["users"]["staff_without_2fa"], 3,
        "the three staff accounts have no 2FA yet"
    );

    let found = app.get("/admin/search?q=alpha", &moderator).await;
    assert_eq!(found.body["users"][0]["username"], "con.alpha");
    assert_eq!(found.body["songs"][0]["title"], "Alpha Song");
    assert_eq!(found.body["songs"][0]["subtitle"], "Alpha Band");
    let short = app.get("/admin/search?q=a", &moderator).await;
    assert!(short.body["users"].as_array().unwrap().is_empty());

    // Filters on the user list.
    let staff = app
        .get("/users?role=moderator&sort=newest", &moderator)
        .await;
    assert_eq!(staff.body["meta"]["total_items"], 2);
    assert_eq!(staff.body["data"][0]["username"], "con.mod2");

    // CSV exports: admins only, audited, formula-safe.
    assert_eq!(
        app.get("/admin/users/export", &moderator).await.status,
        StatusCode::FORBIDDEN
    );
    let csv = app.get("/admin/users/export?role=user", &admin).await;
    assert_eq!(csv.status, StatusCode::OK);
    assert!(
        csv.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    let text = String::from_utf8(csv.bytes.clone()).unwrap();
    let lines: Vec<&str> = text.trim_end().split("\r\n").collect();
    assert!(
        lines[0]
            .trim_start_matches('\u{feff}')
            .starts_with("id,username,email")
    );
    assert_eq!(lines.len(), 3, "a header and the two regular accounts");
    let audit_csv = app
        .get("/admin/audit-logs/export?action=staff.", &admin)
        .await;
    assert_eq!(audit_csv.status, StatusCode::OK);

    // Bulk: what the caller may do is done, the rest is reported.
    let bulk = app
        .post(
            "/admin/users/bulk",
            &moderator,
            json!({
                "action": "ban",
                "user_ids": [target_a, target_b, other_mod],
                "duration_hours": 24,
                "reason": "spam wave",
            }),
        )
        .await;
    assert_eq!(bulk.status, StatusCode::OK, "{}", bulk.body);
    assert_eq!(bulk.body["succeeded"].as_array().unwrap().len(), 2);
    assert_eq!(bulk.body["failed"][0]["user_id"], other_mod.to_string());
    assert_eq!(bulk.body["failed"][0]["code"], "INSUFFICIENT_ROLE");
    let banned = app.get("/users?state=banned", &moderator).await;
    assert_eq!(banned.body["meta"]["total_items"], 2);
    let unban = app
        .post(
            "/admin/users/bulk",
            &moderator,
            json!({ "action": "unban", "user_ids": [target_a, target_b] }),
        )
        .await;
    assert_eq!(unban.body["succeeded"].as_array().unwrap().len(), 2);

    // Sign-in activity.
    app.login_response("con.alpha", "wrong-password").await;
    let user_a = app.login("con.alpha", STRONG_PASSWORD).await;
    // Audit entries of sign-ins are written in the background.
    let mut events = serde_json::Value::Null;
    for _ in 0..50 {
        events = app.get("/users/me/sign-ins", &user_a).await.body;
        if events.as_array().is_some_and(|e| e.len() >= 3) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let events = events.as_array().unwrap();
    assert_eq!(events[0]["outcome"], "succeeded");
    assert_eq!(events[0]["method"], "password");
    assert!(events.iter().any(|e| e["outcome"] == "failed"));
    assert_eq!(
        app.get(&format!("/admin/users/{target_a}/sign-ins"), &moderator)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get(&format!("/admin/users/{target_a}/sign-ins"), &admin)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_platform_update_must_name_every_switch() {
    let app = app!();
    let (_, admin) = app.user("partial.admin", Role::Admin).await;
    app.put(
        "/admin/settings/platform",
        &admin,
        platform("off", false, &["mailinator.com"]),
    )
    .await;

    // Leaving switches out (or misspelling one) must not reset them.
    for body in [
        json!({ "maintenance": { "mode": "read_only" } }),
        json!({
            "maintenance": { "mode": "off" },
            "registration_open": true,
            "blocked_email_domains": [],
        }),
    ] {
        let refused = app.put("/admin/settings/platform", &admin, body).await;
        assert!(refused.status.is_client_error(), "{}", refused.body);
    }
    let current = app.get("/admin/settings/platform", &admin).await;
    assert_eq!(current.body["maintenance"]["mode"], "off");
    assert_eq!(current.body["registrations_open"], false);
    assert_eq!(
        current.body["blocked_email_domains"],
        json!(["mailinator.com"])
    );
}

#[tokio::test]
async fn blocked_domains_match_every_spelling_of_the_domain() {
    let app = app!();
    let (_, admin) = app.user("idn.admin", Role::Admin).await;
    let saved = app
        .put(
            "/admin/settings/platform",
            &admin,
            platform("off", true, &["mailinator.com", "bücher.de"]),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(
        saved.body["blocked_email_domains"],
        json!(["mailinator.com", "xn--bcher-kva.de"])
    );
    for (username, email) in [
        ("idn.fullwidth", "x@ｍａｉｌｉｎａｔｏｒ.com"),
        ("idn.dot", "x@mailinator\u{3002}com"),
        ("idn.unicode", "x@bücher.de"),
    ] {
        let refused = app.register(username, email, json!({})).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{email}");
        assert_eq!(
            refused.code(),
            "EMAIL_DOMAIN_BLOCKED",
            "{email}: {}",
            refused.body
        );
    }
}

#[tokio::test]
async fn support_tickets_close_counting_from_their_resolution() {
    let app = app!();
    let (_, user) = app.user("stale.user", Role::User).await;
    let (_, moderator) = app.user("stale.mod", Role::Moderator).await;
    let created = app
        .post(
            "/support/tickets",
            &user,
            json!({ "subject": "Old question", "category": "other", "body": "Hello?" }),
        )
        .await;
    let ticket_id = created.body["ticket"]["id"].as_str().unwrap().to_string();
    // The last message is weeks old when staff resolve it.
    sqlx::query(
        "UPDATE support_tickets SET last_message_at = last_message_at - INTERVAL '20 days'
         WHERE id = $1::uuid",
    )
    .bind(&ticket_id)
    .execute(&app.pool)
    .await
    .unwrap();
    app.patch(
        &format!("/admin/support/tickets/{ticket_id}"),
        &moderator,
        json!({ "status": "resolved" }),
    )
    .await;
    // The requester still has their 14 days to come back.
    let closed = app
        .state
        .support_repo
        .close_stale_resolved(setlyst_api::controllers::support::RESOLVED_TICKET_CLOSE_DAYS)
        .await
        .unwrap();
    assert_eq!(closed, 0);
    sqlx::query(
        "UPDATE support_tickets SET resolved_at = resolved_at - INTERVAL '15 days'
         WHERE id = $1::uuid",
    )
    .bind(&ticket_id)
    .execute(&app.pool)
    .await
    .unwrap();
    // A fresh staff message that keeps it resolved restarts the wait too.
    let reply = app
        .post(
            &format!("/admin/support/tickets/{ticket_id}/messages"),
            &moderator,
            json!({ "body": "Still resolved, just checking in.", "status": "resolved" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let closed = app
        .state
        .support_repo
        .close_stale_resolved(setlyst_api::controllers::support::RESOLVED_TICKET_CLOSE_DAYS)
        .await
        .unwrap();
    assert_eq!(closed, 0);
    sqlx::query(
        "UPDATE support_tickets SET last_message_at = last_message_at - INTERVAL '15 days'
         WHERE id = $1::uuid",
    )
    .bind(&ticket_id)
    .execute(&app.pool)
    .await
    .unwrap();
    let closed = app
        .state
        .support_repo
        .close_stale_resolved(setlyst_api::controllers::support::RESOLVED_TICKET_CLOSE_DAYS)
        .await
        .unwrap();
    assert_eq!(closed, 1);
}

#[tokio::test]
async fn support_input_and_staff_rules() {
    let app = app!();
    let (_, user) = app.user("rules.user", Role::User).await;
    let (moderator_id, moderator) = app.user("rules.mod", Role::Moderator).await;
    let (inactive_id, _) = app.user("rules.gone", Role::Moderator).await;

    // NUL can't be stored: a 400, not a 500.
    for body in [
        json!({ "subject": "Nul\u{0}", "category": "bug", "body": "x" }),
        json!({ "subject": "Fine subject", "category": "bug", "body": "a\u{0}b" }),
        json!({ "subject": "Fine subject", "category": "bug", "body": "x",
                "context": { "page": "a\u{0}" } }),
    ] {
        let refused = app.post("/support/tickets", &user, body).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    }

    // Anywhere else too (incidents have no NUL check of their own).
    let incident = app
        .post(
            "/admin/incidents",
            &moderator,
            json!({ "kind": "incident", "title": "API \u{0} down", "impact": "minor",
                    "components": ["api"], "message": "Looking into it." }),
        )
        .await;
    assert_eq!(
        incident.status,
        StatusCode::BAD_REQUEST,
        "{}",
        incident.body
    );

    // Staff don't handle their own requests.
    let own = app
        .post(
            "/support/tickets",
            &moderator,
            json!({ "subject": "My own question", "category": "other", "body": "?" }),
        )
        .await;
    let own_id = own.body["ticket"]["id"].as_str().unwrap().to_string();
    let reply = app
        .post(
            &format!("/admin/support/tickets/{own_id}/messages"),
            &moderator,
            json!({ "body": "Solved it myself", "status": "resolved" }),
        )
        .await;
    assert_eq!(reply.code(), "CANNOT_TARGET_SELF", "{}", reply.body);
    let update = app
        .patch(
            &format!("/admin/support/tickets/{own_id}"),
            &moderator,
            json!({ "status": "resolved" }),
        )
        .await;
    assert_eq!(update.code(), "CANNOT_TARGET_SELF", "{}", update.body);

    // Assignees are active staff.
    let created = app
        .post(
            "/support/tickets",
            &user,
            json!({ "subject": "Help please", "category": "other", "body": "?" }),
        )
        .await;
    let ticket_id = created.body["ticket"]["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE users SET status = 'inactive' WHERE id = $1")
        .bind(inactive_id)
        .execute(&app.pool)
        .await
        .unwrap();
    let inactive = app
        .patch(
            &format!("/admin/support/tickets/{ticket_id}"),
            &moderator,
            json!({ "assignee_id": inactive_id }),
        )
        .await;
    assert_eq!(inactive.status, StatusCode::BAD_REQUEST);
    let assigned = app
        .patch(
            &format!("/admin/support/tickets/{ticket_id}"),
            &moderator,
            json!({ "assignee_id": moderator_id }),
        )
        .await;
    assert_eq!(assigned.status, StatusCode::OK, "{}", assigned.body);

    // Unknown inbox filters are refused rather than matching nothing.
    let typo = app
        .get("/admin/support/tickets?status=Open", &moderator)
        .await;
    assert_eq!(typo.status, StatusCode::BAD_REQUEST);
    let open = app
        .get("/admin/support/tickets?status=open", &moderator)
        .await;
    assert_eq!(open.status, StatusCode::OK);
    // Same for the account list (and its export): a misspelt filter
    // would list every account.
    let users = app.get("/users?state=suspended", &moderator).await;
    assert_eq!(users.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.get("/users?state=active&sort=newest", &moderator)
            .await
            .status,
        StatusCode::OK
    );

    // Reading a conversation is recorded.
    app.get(&format!("/admin/support/tickets/{ticket_id}"), &moderator)
        .await;
    let viewed = app
        .wait_for_count(
            "SELECT COUNT(*) FROM audit_logs WHERE actor_id = $1
               AND action = 'staff.content_viewed' AND target_type = 'support_ticket'",
            moderator_id,
            1,
        )
        .await;
    assert_eq!(viewed, 1);
}
