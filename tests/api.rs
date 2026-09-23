//! End-to-end tests of the HTTP API against a real PostgreSQL database.
//! See `tests/common/mod.rs` for how the environment is set up.

mod common;

use axum::http::StatusCode;
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

// ---------------------------------------------------------------------
// Authentication & password policy
// ---------------------------------------------------------------------

#[tokio::test]
async fn registration_enforces_the_password_and_username_policies() {
    let app = app!();

    let weak = app
        .request(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(json!({ "username": "newbie", "email": "newbie@example.com", "accept_terms": true, "password": "password" })),
        )
        .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    assert_eq!(weak.code(), "WEAK_PASSWORD");
    let issues = weak.body["meta"]["issues"].as_array().unwrap();
    assert!(issues.iter().any(|i| i == "missing_uppercase"));

    let contains_username = app
        .request(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(json!({ "username": "newbie", "email": "newbie@example.com", "accept_terms": true, "password": "Newbie#2026" })),
        )
        .await;
    assert_eq!(contains_username.code(), "WEAK_PASSWORD");

    let reserved = app
        .request(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(json!({ "username": "admin", "email": "admin@example.com", "accept_terms": true, "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(reserved.status, StatusCode::BAD_REQUEST);
    assert_eq!(reserved.code(), "VALIDATION_ERROR");

    let ok = app
        .request(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(json!({ "username": "newbie", "password": STRONG_PASSWORD, "email": "newbie@example.com", "accept_terms": true })),
        )
        .await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
    assert!(ok.body.get("password_hash").is_none());

    let duplicate = app
        .request(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(json!({ "username": "NEWBIE", "email": "other@example.com", "accept_terms": true, "password": STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(duplicate.code(), "USERNAME_TAKEN");
}

#[tokio::test]
async fn login_does_not_reveal_whether_an_account_exists() {
    let app = app!();
    app.create_user("existing", STRONG_PASSWORD, Role::User)
        .await;

    let unknown = app.login_response("nobody", STRONG_PASSWORD).await;
    let wrong = app.login_response("existing", "Wr0ng!Password").await;

    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.code(), "INVALID_CREDENTIALS");
    assert_eq!(wrong.code(), "INVALID_CREDENTIALS");
    assert_eq!(unknown.body["message"], wrong.body["message"]);
}

#[tokio::test]
async fn a_legacy_weak_password_forces_a_change_before_anything_else() {
    let app = app!();
    app.create_user("legacy", "weakpass", Role::User).await;

    let login = app.login_response("legacy", "weakpass").await;
    assert_eq!(login.status, StatusCode::OK);
    assert_eq!(login.body["must_change_password"], true);
    let token = login.body["token"].as_str().unwrap().to_string();

    // Everything but reading yourself and changing the password is blocked.
    let blocked = app.get("/songs", &token).await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN);
    assert_eq!(blocked.code(), "PASSWORD_CHANGE_REQUIRED");
    assert_eq!(app.get("/users/me", &token).await.status, StatusCode::OK);

    let reused = app
        .patch(
            "/users/me/password",
            &token,
            json!({ "current_password": "weakpass", "new_password": "weakpass" }),
        )
        .await;
    assert_eq!(reused.code(), "PASSWORD_REUSED");

    let still_weak = app
        .patch(
            "/users/me/password",
            &token,
            json!({ "current_password": "weakpass", "new_password": "stillweak" }),
        )
        .await;
    assert_eq!(still_weak.code(), "WEAK_PASSWORD");

    let changed = app
        .patch(
            "/users/me/password",
            &token,
            json!({ "current_password": "weakpass", "new_password": STRONG_PASSWORD }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.body["reauth_required"], true);

    // The old session is revoked; the new password works and is compliant.
    let revoked = app.get("/users/me", &token).await;
    assert_eq!(revoked.status, StatusCode::UNAUTHORIZED);
    assert_eq!(revoked.code(), "SESSION_REVOKED");

    let relogin = app.login_response("legacy", STRONG_PASSWORD).await;
    assert_eq!(relogin.body["must_change_password"], false);
}

#[tokio::test]
async fn changing_the_password_works_after_a_username_change() {
    let app = app!();
    let (_, token) = app.user("before", Role::User).await;

    let renamed = app
        .patch("/users/me", &token, json!({ "username": "after" }))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);

    // The token still carries the old username; the change must not care.
    let changed = app
        .patch(
            "/users/me/password",
            &token,
            json!({ "current_password": STRONG_PASSWORD, "new_password": "An0ther!Secret" }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
}

// ---------------------------------------------------------------------
// Moderation
// ---------------------------------------------------------------------

#[tokio::test]
async fn moderators_can_never_act_on_admins_or_peers() {
    let app = app!();
    let (admin_id, admin) = app.user("boss", Role::Admin).await;
    let (other_mod_id, _) = app.user("mod.two", Role::Moderator).await;
    let (user_id, _) = app.user("regular", Role::User).await;
    let (_, moderator) = app.user("mod.one", Role::Moderator).await;

    for target in [admin_id, other_mod_id] {
        let delete = app.delete(&format!("/users/{target}"), &moderator).await;
        assert_eq!(delete.status, StatusCode::FORBIDDEN);
        assert_eq!(delete.code(), "INSUFFICIENT_ROLE");

        let ban = app
            .post(&format!("/users/{target}/ban"), &moderator, json!({}))
            .await;
        assert_eq!(ban.code(), "INSUFFICIENT_ROLE");
    }

    let promote = app
        .patch(
            &format!("/users/{user_id}"),
            &moderator,
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(promote.status, StatusCode::FORBIDDEN);

    let create_admin = app
        .post(
            "/users",
            &moderator,
            json!({ "username": "sneaky", "password": STRONG_PASSWORD, "role": "admin" }),
        )
        .await;
    assert_eq!(create_admin.status, StatusCode::FORBIDDEN);

    let self_delete = app.delete(&format!("/users/{admin_id}"), &admin).await;
    assert_eq!(self_delete.code(), "CANNOT_TARGET_SELF");

    // Moderators can manage regular users.
    let deactivate = app
        .patch(
            &format!("/users/{user_id}"),
            &moderator,
            json!({ "status": "inactive" }),
        )
        .await;
    assert_eq!(deactivate.status, StatusCode::OK, "{}", deactivate.body);
    assert_eq!(deactivate.body["status"], "inactive");
}

#[tokio::test]
async fn suspensions_lock_the_account_out_immediately() {
    let app = app!();
    let (_, moderator) = app.user("warden", Role::Moderator).await;
    let (user_id, user) = app.user("troublemaker", Role::User).await;

    assert_eq!(app.get("/songs", &user).await.status, StatusCode::OK);

    let ban = app
        .post(
            &format!("/users/{user_id}/ban"),
            &moderator,
            json!({ "duration_hours": 24, "reason": "Spam" }),
        )
        .await;
    assert_eq!(ban.status, StatusCode::OK, "{}", ban.body);
    assert_eq!(ban.body["is_banned"], true);
    assert_eq!(ban.body["ban_reason"], "Spam");

    // Existing session is gone...
    assert_eq!(app.get("/songs", &user).await.code(), "SESSION_REVOKED");
    // ...and signing in explains why (only after a correct password).
    let login = app.login_response("troublemaker", STRONG_PASSWORD).await;
    assert_eq!(login.status, StatusCode::FORBIDDEN);
    assert_eq!(login.code(), "ACCOUNT_BANNED");
    assert_eq!(login.body["meta"]["reason"], "Spam");
    let wrong = app.login_response("troublemaker", "Wr0ng!Password").await;
    assert_eq!(wrong.code(), "INVALID_CREDENTIALS");

    let unban = app
        .delete(&format!("/users/{user_id}/ban"), &moderator)
        .await;
    assert_eq!(unban.status, StatusCode::OK);
    assert_eq!(unban.body["is_banned"], false);
    assert_eq!(
        app.login_response("troublemaker", STRONG_PASSWORD)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn deactivated_accounts_cannot_sign_in() {
    let app = app!();
    let (_, admin) = app.user("chief", Role::Admin).await;
    let (user_id, user) = app.user("dormant", Role::User).await;

    app.patch(
        &format!("/users/{user_id}"),
        &admin,
        json!({ "status": "inactive" }),
    )
    .await;

    assert_eq!(app.get("/songs", &user).await.code(), "ACCOUNT_DEACTIVATED");
    assert_eq!(
        app.login_response("dormant", STRONG_PASSWORD).await.code(),
        "ACCOUNT_DEACTIVATED"
    );
}

#[tokio::test]
async fn admin_password_reset_revokes_sessions_and_forces_a_change() {
    let app = app!();
    let (_, admin) = app.user("keeper", Role::Admin).await;
    let (user_id, user) = app.user("forgetful", Role::User).await;

    let weak = app
        .post(
            &format!("/users/{user_id}/password-reset"),
            &admin,
            json!({ "new_password": "123" }),
        )
        .await;
    assert_eq!(weak.code(), "WEAK_PASSWORD");

    let reset = app
        .post(
            &format!("/users/{user_id}/password-reset"),
            &admin,
            json!({ "new_password": "Temp#Pass2026" }),
        )
        .await;
    assert_eq!(reset.status, StatusCode::NO_CONTENT);

    assert_eq!(app.get("/songs", &user).await.code(), "SESSION_REVOKED");
    let login = app.login_response("forgetful", "Temp#Pass2026").await;
    assert_eq!(login.body["must_change_password"], true);
}

#[tokio::test]
async fn impersonation_is_read_only_and_hierarchical() {
    let app = app!();
    let (admin_id, admin) = app.user("overseer", Role::Admin).await;
    let (_, moderator) = app.user("helper", Role::Moderator).await;
    let (user_id, user) = app.user("customer", Role::User).await;
    let artist = app.artist(&user, "Artist").await;
    app.song(&user, &artist, "Private Song").await;

    let denied = app
        .post(
            &format!("/users/{admin_id}/impersonate"),
            &moderator,
            json!({}),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let issued = app
        .post(&format!("/users/{user_id}/impersonate"), &admin, json!({}))
        .await;
    assert_eq!(issued.status, StatusCode::OK, "{}", issued.body);
    let token = issued.body["token"].as_str().unwrap();

    let me = app.get("/users/me", token).await;
    assert_eq!(me.body["username"], "customer");
    let songs = app.get("/songs", token).await;
    assert_eq!(songs.body["meta"]["total_items"], 1);

    let write = app.post("/artists", token, json!({ "name": "Nope" })).await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);
    assert_eq!(write.code(), "IMPERSONATION_READ_ONLY");

    // Recorded in the audit log.
    let log = app
        .get("/admin/audit-logs?action=user.impersonated", &admin)
        .await;
    assert_eq!(log.body["meta"]["total_items"], 1);
    assert_eq!(log.body["data"][0]["target_label"], "customer");
}

// ---------------------------------------------------------------------
// Quotas
// ---------------------------------------------------------------------

#[tokio::test]
async fn per_user_quotas_are_enforced_and_reported() {
    let app = app!();
    let (_, admin) = app.user("limiter", Role::Admin).await;
    let (user_id, user) = app.user("hoarder", Role::User).await;

    let set = app
        .put(
            &format!("/users/{user_id}/quotas"),
            &admin,
            json!({ "overrides": { "songs": 1, "setlists": 0 }, "unlimited": false }),
        )
        .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);

    let artist = app.artist(&user, "Band").await;
    assert_eq!(
        app.song(&user, &artist, "One").await.status,
        StatusCode::CREATED
    );
    let second = app.song(&user, &artist, "Two").await;
    assert_eq!(second.status, StatusCode::FORBIDDEN);
    assert_eq!(second.code(), "QUOTA_EXCEEDED");
    assert_eq!(second.body["meta"]["resource"], "songs");
    assert_eq!(second.body["meta"]["limit"], 1);

    let setlist = app
        .post("/setlists", &user, json!({ "title": "Blocked" }))
        .await;
    assert_eq!(setlist.code(), "QUOTA_EXCEEDED");

    let report = app.get("/users/me/quotas", &user).await;
    let songs = report.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["resource"] == "songs")
        .unwrap()
        .clone();
    assert_eq!(songs["used"], 1);
    assert_eq!(songs["limit"], 1);
    assert_eq!(songs["overridden"], true);

    // Admins are never limited.
    let admin_report = app.get("/users/me/quotas", &admin).await;
    assert_eq!(admin_report.body["unlimited"], true);

    // Only admins can change limits.
    let (_, moderator) = app.user("notallowed", Role::Moderator).await;
    let forbidden = app
        .put(
            &format!("/users/{user_id}/quotas"),
            &moderator,
            json!({ "overrides": {}, "unlimited": true }),
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn platform_default_quotas_apply_to_everyone() {
    let app = app!();
    let (_, admin) = app.user("platform", Role::Admin).await;
    let (_, user) = app.user("somebody", Role::User).await;

    let mut defaults = app.get("/admin/settings/quotas", &admin).await.body;
    defaults["artists"] = json!(1);
    let saved = app.put("/admin/settings/quotas", &admin, defaults).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    app.artist(&user, "First").await;
    let second = app
        .post("/artists", &user, json!({ "name": "Second" }))
        .await;
    assert_eq!(second.code(), "QUOTA_EXCEEDED");
}

// ---------------------------------------------------------------------
// Tags & clearable fields
// ---------------------------------------------------------------------

#[tokio::test]
async fn songs_can_be_tagged_searched_and_cleared() {
    let app = app!();
    let (_, user) = app.user("tagger", Role::User).await;
    let artist = app.artist(&user, "Djavan").await;

    let created = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Oceano",
                "artist_id": artist,
                "tempo": 80,
                "tags": [" Romântica ", "BALADA", "balada"]
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["tags"], json!(["romântica", "balada"]));
    let song_id = created.body["id"].as_str().unwrap().to_string();

    app.post(
        "/songs",
        &user,
        json!({ "title": "Flor de Lis", "artist_id": artist, "tags": ["animada"] }),
    )
    .await;

    let by_tag = app.get("/songs?tags=balada", &user).await;
    assert_eq!(by_tag.body["meta"]["total_items"], 1);
    assert_eq!(by_tag.body["data"][0]["title"], "Oceano");

    let by_search = app.get("/songs?q=flor", &user).await;
    assert_eq!(by_search.body["meta"]["total_items"], 1);

    let tags = app.get("/songs/tags", &user).await;
    assert_eq!(tags.body.as_array().unwrap().len(), 3);

    let renamed = app
        .patch("/songs/tags/balada", &user, json!({ "new_name": "Lenta" }))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    assert_eq!(
        app.get("/songs?tags=lenta", &user).await.body["meta"]["total_items"],
        1
    );

    let too_many: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
    let rejected = app
        .patch(
            &format!("/songs/{song_id}"),
            &user,
            json!({ "tags": too_many }),
        )
        .await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);

    // `null` clears a nullable field (it used to be silently ignored).
    let cleared = app
        .patch(
            &format!("/songs/{song_id}"),
            &user,
            json!({ "tempo": null }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    let song = app.get(&format!("/songs/{song_id}"), &user).await;
    assert!(song.body["tempo"].is_null());
    assert!(song.body["updated_by_username"] == "tagger");
}

// ---------------------------------------------------------------------
// Public links moderation
// ---------------------------------------------------------------------

#[tokio::test]
async fn staff_can_take_public_links_down_and_owners_cannot_reshare() {
    let app = app!();
    let (_, moderator) = app.user("sheriff", Role::Moderator).await;
    let (_, user) = app.user("sharer", Role::User).await;
    let setlist = app.setlist(&user, "Public set", None).await;

    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    let token = shared.body["share_token"].as_str().unwrap().to_string();
    let public_path = format!("/public/setlists/{token}");
    assert_eq!(
        app.request(axum::http::Method::GET, &public_path, None, None)
            .await
            .status,
        StatusCode::OK
    );

    let links = app.get("/admin/shared-links", &moderator).await;
    assert_eq!(links.body["meta"]["total_items"], 1);

    let revoked = app
        .post(
            &format!("/admin/setlists/{setlist}/share/revoke"),
            &moderator,
            json!({ "reason": "Copyrighted material" }),
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);

    assert_eq!(
        app.request(axum::http::Method::GET, &public_path, None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let reshare = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    assert_eq!(reshare.status, StatusCode::FORBIDDEN);
    assert_eq!(reshare.code(), "SHARE_LOCKED");

    let detail = app.get(&format!("/setlists/{setlist}"), &user).await;
    assert_eq!(detail.body["share_lock_reason"], "Copyrighted material");

    let notifications = app.get("/notifications", &user).await;
    assert!(
        notifications
            .body
            .to_string()
            .contains("share_link_revoked")
    );

    app.post(
        &format!("/admin/setlists/{setlist}/share/unlock"),
        &moderator,
        json!({}),
    )
    .await;
    let reshared = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    assert_eq!(reshared.status, StatusCode::OK);
    assert_ne!(reshared.body["share_token"], token.as_str());
}

// ---------------------------------------------------------------------
// Bands & account deletion
// ---------------------------------------------------------------------

#[tokio::test]
async fn admins_manage_any_band_and_deleting_an_owner_keeps_the_band() {
    let app = app!();
    let (_, admin) = app.user("director", Role::Admin).await;
    let (owner_id, owner) = app.user("frontman", Role::User).await;
    let (member_id, member) = app.user("drummer", Role::User).await;

    let band = app
        .post("/bands", &owner, json!({ "name": "AC/DC Tribute" }))
        .await;
    assert_eq!(band.status, StatusCode::CREATED, "{}", band.body);
    let band_id = band.body["id"].as_str().unwrap().to_string();

    let band_setlist = app.setlist(&owner, "Band set", Some(&band_id)).await;

    // Admin adds a user straight into the band.
    let added = app
        .post(
            &format!("/admin/bands/{band_id}/members"),
            &admin,
            json!({ "user_id": member_id, "role": "moderator" }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    let again = app
        .post(
            &format!("/admin/bands/{band_id}/members"),
            &admin,
            json!({ "user_id": member_id }),
        )
        .await;
    assert_eq!(again.code(), "ALREADY_MEMBER");

    let bands = app.get("/admin/bands?q=tribute", &admin).await;
    assert_eq!(bands.body["meta"]["total_items"], 1);
    assert_eq!(bands.body["data"][0]["member_count"], 2);

    // Deleting the owner hands the band to the remaining member instead of
    // failing (or cascading the band's setlists away).
    let deleted = app.delete(&format!("/users/{owner_id}"), &admin).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    let detail = app.get(&format!("/admin/bands/{band_id}"), &admin).await;
    assert_eq!(detail.body["band"]["owner_id"], member_id.to_string());
    let setlist = app.get(&format!("/setlists/{band_setlist}"), &member).await;
    assert_eq!(setlist.status, StatusCode::OK);
}

#[tokio::test]
async fn band_invites_cannot_be_over_redeemed() {
    let app = app!();
    let (_, owner) = app.user("leader", Role::User).await;
    let (_, first) = app.user("first.in", Role::User).await;
    let (_, second) = app.user("second.in", Role::User).await;

    let band_id = app
        .post("/bands", &owner, json!({ "name": "Invite Test" }))
        .await
        .body["id"]
        .as_str()
        .unwrap()
        .to_string();

    let invite = app
        .post(
            &format!("/bands/{band_id}/invites"),
            &owner,
            json!({ "max_uses": 1 }),
        )
        .await;
    let code = invite.body["code"].as_str().unwrap().to_string();

    let accepted = app
        .post(&format!("/invites/{code}/accept"), &first, json!({}))
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let exhausted = app
        .post(&format!("/invites/{code}/accept"), &second, json!({}))
        .await;
    assert_eq!(exhausted.status, StatusCode::NOT_FOUND);
    assert_eq!(exhausted.code(), "INVITE_INVALID");
}

// ---------------------------------------------------------------------
// Exports, preferences, status
// ---------------------------------------------------------------------

#[tokio::test]
async fn pdf_export_honours_options_and_names_the_file() {
    let app = app!();
    let (_, user) = app.user("printer", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let song = app.song(&user, &artist, "Canção").await;
    let setlist = app.setlist(&user, "Canções de Sábado", None).await;
    app.post(
        &format!("/setlists/{setlist}/songs"),
        &user,
        json!({ "song_id": song.body["id"] }),
    )
    .await;

    let pdf = app
        .get(
            &format!(
                "/setlists/{setlist}/export/pdf?compact=true&columns=2&include_lyrics=true&watermark=false&lang=pt-BR"
            ),
            &user,
        )
        .await;
    assert_eq!(pdf.status, StatusCode::OK);
    assert_eq!(pdf.headers["content-type"], "application/pdf");
    assert!(pdf.bytes.starts_with(b"%PDF"));
    let disposition = pdf.headers["content-disposition"].to_str().unwrap();
    assert!(disposition.contains("filename*=UTF-8''setlist-can%C3%A7%C3%B5es-de-s%C3%A1bado.pdf"));
}

#[tokio::test]
async fn ui_settings_are_shallow_merged() {
    let app = app!();
    let (_, user) = app.user("tweaker", Role::User).await;

    app.patch(
        "/users/me/preferences",
        &user,
        json!({ "ui_settings": { "live": { "compact": true }, "pdf": { "watermark": false } } }),
    )
    .await;
    let merged = app
        .patch(
            "/users/me/preferences",
            &user,
            json!({ "theme": "dark", "ui_settings": { "live": { "compact": false }, "pdf": null } }),
        )
        .await;
    assert_eq!(merged.status, StatusCode::OK, "{}", merged.body);
    assert_eq!(merged.body["theme"], "dark");
    assert_eq!(
        merged.body["ui_settings"],
        json!({ "live": { "compact": false } })
    );

    let invalid = app
        .patch(
            "/users/me/preferences",
            &user,
            json!({ "language": "klingon" }),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn status_reports_health_version_and_database() {
    let app = app!();
    let status = app
        .request(axum::http::Method::GET, "/status", None, None)
        .await;
    assert_eq!(status.status, StatusCode::OK);
    assert!(matches!(
        status.body["status"].as_str(),
        Some("operational" | "degraded")
    ));
    assert_eq!(status.body["version"], env!("CARGO_PKG_VERSION"));
    // Infrastructure details are staff-only (v0.12).
    assert!(status.body.get("dependencies").is_none());
    let (_, staff) = app.user("status.mod", Role::Moderator).await;
    let details = app.get("/status/details", &staff).await;
    assert!(details.body["dependencies"]["database"]["latency_ms"].is_number());
}

#[tokio::test]
async fn admins_can_manage_any_users_content() {
    let app = app!();
    let (_, admin) = app.user("curator", Role::Admin).await;
    let (_, moderator) = app.user("viewer", Role::Moderator).await;
    let (_, user) = app.user("musician", Role::User).await;
    let artist = app.artist(&user, "Artist").await;
    let song = app.song(&user, &artist, "Mine").await;
    let song_id = song.body["id"].as_str().unwrap().to_string();

    let listed = app.get("/admin/songs?q=musician", &moderator).await;
    assert_eq!(listed.body["meta"]["total_items"], 1);

    let detail = app
        .get(&format!("/admin/songs/{song_id}"), &moderator)
        .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(detail.body["song"]["title"], "Mine");
    assert_eq!(detail.body["summary"]["owner_username"], "musician");

    // Moderators can look but not touch.
    let mod_delete = app
        .delete(&format!("/admin/songs/{song_id}"), &moderator)
        .await;
    assert_eq!(mod_delete.status, StatusCode::FORBIDDEN);

    let edited = app
        .patch(
            &format!("/admin/songs/{song_id}"),
            &admin,
            json!({ "title": "Fixed Title", "tags": ["corrigida"] }),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    assert_eq!(edited.body["updated_by_username"], "curator");

    let deleted = app.delete(&format!("/admin/songs/{song_id}"), &admin).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/songs/{song_id}"), &user).await.status,
        StatusCode::NOT_FOUND
    );

    let log = app.get("/admin/audit-logs?action=song.", &admin).await;
    assert_eq!(log.body["meta"]["total_items"], 2);
}

#[tokio::test]
async fn staff_can_search_users() {
    let app = app!();
    let (_, moderator) = app.user("mod.search", Role::Moderator).await;
    app.user("alice.keys", Role::User).await;
    app.user("bob_drums", Role::User).await;

    let found = app.get("/users?q=KEYS", &moderator).await;
    assert_eq!(found.status, StatusCode::OK, "{}", found.body);
    assert_eq!(found.body["meta"]["total_items"], 1);
    assert_eq!(found.body["data"][0]["username"], "alice.keys");

    // `_` is a literal, not an ILIKE wildcard.
    let literal = app.get("/users?q=b_d", &moderator).await;
    assert_eq!(literal.body["meta"]["total_items"], 1);
    let none = app.get("/users?q=b_x", &moderator).await;
    assert_eq!(none.body["meta"]["total_items"], 0);

    let all = app.get("/users", &moderator).await;
    assert_eq!(all.body["meta"]["total_items"], 3);
}
