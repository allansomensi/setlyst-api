//! Automatic flags, user reports and the moderation queue.

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

const OPEN_FLAGS: &str =
    "SELECT COUNT(*) FROM moderation_flags WHERE user_id = $1 AND status = 'open'";

#[tokio::test]
async fn offensive_usernames_and_avatars_are_flagged_and_slurs_refused() {
    let app = app!();

    let slur = app
        .register("big.faggot", "slur@example.com", json!({}))
        .await;
    assert_eq!(slur.status, StatusCode::BAD_REQUEST);
    assert_eq!(slur.code(), "VALIDATION_ERROR");

    let (rude_id, rude) = app.registered_user("fuuck.band", "rude@example.com").await;
    assert_eq!(app.wait_for_count(OPEN_FLAGS, rude_id, 1).await, 1);

    // Adult sites are refused outright; suspicious paths are flagged.
    let blocked = app
        .patch(
            "/users/me",
            &rude,
            json!({ "avatar_url": "https://www.pornhub.com/me.jpg" }),
        )
        .await;
    assert_eq!(blocked.code(), "INVALID_IMAGE_URL");
    let suspicious = app
        .patch(
            "/users/me",
            &rude,
            json!({ "avatar_url": "https://img.example.com/nsfw/me.jpg" }),
        )
        .await;
    assert_eq!(suspicious.status, StatusCode::OK, "{}", suspicious.body);
    assert_eq!(app.wait_for_count(OPEN_FLAGS, rude_id, 2).await, 2);

    let (_, moderator) = app.user("queue.mod", Role::Moderator).await;
    let summary = app.get("/admin/moderation/summary", &moderator).await;
    assert_eq!(summary.body["open_total"], 2);
    assert_eq!(summary.body["by_type"]["username"], 1);
    assert_eq!(summary.body["by_type"]["avatar"], 1);

    // The queue can be narrowed to one account.
    let for_user = app
        .get(
            &format!("/admin/moderation/flags?status=all&user_id={rude_id}"),
            &moderator,
        )
        .await;
    assert_eq!(for_user.status, StatusCode::OK, "{}", for_user.body);
    assert_eq!(for_user.body["meta"]["total_items"], 2);
    let (other_id, _) = app.user("queue.other", Role::User).await;
    let for_other = app
        .get(
            &format!("/admin/moderation/flags?status=all&user_id={other_id}"),
            &moderator,
        )
        .await;
    assert_eq!(for_other.body["meta"]["total_items"], 0);
    assert_eq!(for_other.body["data"].as_array().unwrap().len(), 0);

    let queue = app
        .get("/admin/moderation/flags?target_type=username", &moderator)
        .await;
    assert_eq!(queue.body["meta"]["total_items"], 1);
    let flag = &queue.body["data"][0];
    assert_eq!(flag["source"], "automatic");
    assert_eq!(flag["user"]["username"], "fuuck.band");
    assert_eq!(flag["current_value"], "fuuck.band");
    assert!(
        flag["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("offensive_term"))
    );

    // Reset the username: random name, history, notification, and the
    // owner can rename right away.
    let flag_id = flag["id"].as_str().unwrap();
    let resolved = app
        .post(
            &format!("/admin/moderation/flags/{flag_id}/resolve"),
            &moderator,
            json!({ "action": "reset_username", "note": "Nome ofensivo.", "notify_user": true }),
        )
        .await;
    assert_eq!(resolved.status, StatusCode::OK, "{}", resolved.body);
    assert_eq!(resolved.body["status"], "actioned");
    assert_eq!(resolved.body["resolution"], "username_reset");
    assert_eq!(resolved.body["resolved_by_username"], "queue.mod");
    let me = app.get("/users/me", &rude).await;
    let new_name = me.body["username"].as_str().unwrap();
    assert!(new_name.starts_with("user-"), "{new_name}");
    assert!(me.body["username_changed_at"].is_null());
    let renamed = app
        .patch("/users/me", &rude, json!({ "username": "nice.band" }))
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    let notifications = app.get("/notifications", &rude).await;
    assert_eq!(notifications.body["data"][0]["type"], "moderation_action");
    assert_eq!(
        notifications.body["data"][0]["data"]["action"],
        "username_reset"
    );

    // Remove the avatar.
    let avatar_flag = app
        .get("/admin/moderation/flags?target_type=avatar", &moderator)
        .await;
    let avatar_id = avatar_flag.body["data"][0]["id"].as_str().unwrap();
    let wrong_action = app
        .post(
            &format!("/admin/moderation/flags/{avatar_id}/resolve"),
            &moderator,
            json!({ "action": "remove_band_logo" }),
        )
        .await;
    assert_eq!(wrong_action.status, StatusCode::BAD_REQUEST);
    let removed = app
        .post(
            &format!("/admin/moderation/flags/{avatar_id}/resolve"),
            &moderator,
            json!({ "action": "remove_avatar" }),
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert!(app.get("/users/me", &rude).await.body["avatar_url"].is_null());
    let again = app
        .post(
            &format!("/admin/moderation/flags/{avatar_id}/resolve"),
            &moderator,
            json!({ "action": "dismiss" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "already resolved");
    assert_eq!(again.code(), "FLAG_ALREADY_RESOLVED");

    let audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action IN ('moderation.username_reset', 'moderation.avatar_removed')",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audit, 2);
}

#[tokio::test]
async fn reports_create_flags_and_moderators_cannot_act_on_staff() {
    let app = app!();
    let (_, reporter) = app
        .registered_user("reporter", "reporter@example.com")
        .await;
    let (target_id, _) = app.registered_user("target", "target@example.com").await;
    let admin_id = app
        .create_user("the.admin", STRONG_PASSWORD, Role::Admin)
        .await;
    let (moderator_id, moderator) = app.user("rep.mod", Role::Moderator).await;

    let reported = app
        .post(
            &format!("/users/{target_id}/report"),
            &reporter,
            json!({ "reason": "impersonation", "details": "Se passa por outro músico." }),
        )
        .await;
    assert_eq!(reported.status, StatusCode::CREATED, "{}", reported.body);
    let duplicate = app
        .post(
            &format!("/users/{target_id}/report"),
            &reporter,
            json!({ "reason": "spam" }),
        )
        .await;
    assert_eq!(duplicate.code(), "ALREADY_EXISTS");
    let own = app
        .post(
            &format!("/users/{moderator_id}/report"),
            &moderator,
            json!({ "reason": "spam" }),
        )
        .await;
    assert_eq!(own.code(), "CANNOT_TARGET_SELF");
    let bad_reason = app
        .post(
            &format!("/users/{target_id}/report"),
            &moderator,
            json!({ "reason": "boring" }),
        )
        .await;
    assert_eq!(bad_reason.status, StatusCode::UNPROCESSABLE_ENTITY);

    let queue = app.get("/admin/moderation/flags", &moderator).await;
    let report = queue.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["source"] == "report")
        .expect("report flag")
        .clone();
    assert_eq!(report["target_type"], "profile");
    assert_eq!(report["reported_by_username"], "reporter");
    assert_eq!(report["report_note"], "Se passa por outro músico.");

    // The target's profile shows open flags to staff only.
    let as_staff = app
        .get(&format!("/users/{target_id}/profile"), &moderator)
        .await;
    assert_eq!(as_staff.body["admin_details"]["open_flags"], 1);
    let as_user = app
        .get(&format!("/users/{target_id}/profile"), &reporter)
        .await;
    assert!(as_user.body["admin_details"].is_null());
    assert_eq!(as_user.body["is_self"], false);

    // A report about an admin can't be handled by a moderator.
    let about_admin = app
        .post(
            &format!("/users/{admin_id}/report"),
            &reporter,
            json!({ "reason": "offensive_username" }),
        )
        .await;
    assert_eq!(about_admin.status, StatusCode::CREATED);
    let queue = app
        .get("/admin/moderation/flags?target_type=username", &moderator)
        .await;
    let admin_flag = queue.body["data"][0]["id"].as_str().unwrap().to_string();
    let refused = app
        .post(
            &format!("/admin/moderation/flags/{admin_flag}/resolve"),
            &moderator,
            json!({ "action": "reset_username" }),
        )
        .await;
    assert_eq!(refused.code(), "INSUFFICIENT_ROLE");

    // Dismissing closes the report.
    let dismissed = app
        .post(
            &format!(
                "/admin/moderation/flags/{}/resolve",
                report["id"].as_str().unwrap()
            ),
            &moderator,
            json!({ "action": "dismiss", "note": "Conta legítima." }),
        )
        .await;
    assert_eq!(dismissed.body["status"], "dismissed");
    let closed = app
        .get("/admin/moderation/flags?status=dismissed", &moderator)
        .await;
    assert_eq!(closed.body["meta"]["total_items"], 1);

    // Rescans are admin-only.
    assert_eq!(
        app.post("/admin/moderation/rescan", &moderator, json!({}))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let admin_token = app.login("the.admin", STRONG_PASSWORD).await;
    sqlx::query("UPDATE users SET username = 'shit.happens' WHERE id = $1")
        .bind(target_id)
        .execute(&app.pool)
        .await
        .unwrap();
    let rescan = app
        .post("/admin/moderation/rescan", &admin_token, json!({}))
        .await;
    assert_eq!(rescan.status, StatusCode::OK, "{}", rescan.body);
    assert_eq!(rescan.body["flagged"], 1);
    // Running it again adds nothing (one open flag per value).
    let rescan = app
        .post("/admin/moderation/rescan", &admin_token, json!({}))
        .await;
    assert_eq!(rescan.body["flagged"], 0);
}

#[tokio::test]
async fn staff_cannot_close_flags_about_themselves_except_admins_dismissing() {
    let app = app!();
    let (_, reporter) = app
        .registered_user("selfreporter", "selfreporter@example.com")
        .await;
    let (moderator_id, moderator) = app.user("self.mod", Role::Moderator).await;
    let (admin_id, admin) = app.user("self.admin", Role::Admin).await;

    let flag_about = |queue: &serde_json::Value, user_id: uuid::Uuid| -> String {
        queue["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["user"]["id"] == user_id.to_string())
            .unwrap_or_else(|| panic!("no flag about {user_id}: {queue}"))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    for target in [moderator_id, admin_id] {
        let reported = app
            .post(
                &format!("/users/{target}/report"),
                &reporter,
                json!({ "reason": "spam" }),
            )
            .await;
        assert_eq!(reported.status, StatusCode::CREATED, "{}", reported.body);
    }

    // A moderator can't dismiss (or act on) a report about themselves.
    let queue = app.get("/admin/moderation/flags", &moderator).await;
    let own_flag = flag_about(&queue.body, moderator_id);
    for action in ["dismiss", "reset_username"] {
        let refused = app
            .post(
                &format!("/admin/moderation/flags/{own_flag}/resolve"),
                &moderator,
                json!({ "action": action }),
            )
            .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        assert_eq!(refused.code(), "CANNOT_TARGET_SELF");
    }
    // An admin (who outranks the moderator) can.
    let by_admin = app
        .post(
            &format!("/admin/moderation/flags/{own_flag}/resolve"),
            &admin,
            json!({ "action": "dismiss" }),
        )
        .await;
    assert_eq!(by_admin.status, StatusCode::OK, "{}", by_admin.body);

    // An admin may dismiss a report about themselves (nobody outranks
    // them), but never take action on their own account.
    let queue = app.get("/admin/moderation/flags", &admin).await;
    let admin_flag = flag_about(&queue.body, admin_id);
    let refused = app
        .post(
            &format!("/admin/moderation/flags/{admin_flag}/resolve"),
            &admin,
            json!({ "action": "reset_username" }),
        )
        .await;
    assert_eq!(refused.code(), "CANNOT_TARGET_SELF");
    let dismissed = app
        .post(
            &format!("/admin/moderation/flags/{admin_flag}/resolve"),
            &admin,
            json!({ "action": "dismiss" }),
        )
        .await;
    assert_eq!(dismissed.status, StatusCode::OK, "{}", dismissed.body);
    assert_eq!(dismissed.body["status"], "dismissed");
}
