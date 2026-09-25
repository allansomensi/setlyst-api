//! Bands: repertoire, song suggestions and votes, reminders, role
//! escalation and invites.

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::json;
use setlyst_api::models::{quota::QuotaOverrides, user::Role};

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn every_band_has_a_protected_repertoire_that_collects_its_songs() {
    let app = app!();
    let (_, owner) = app.user("repowner", Role::User).await;
    let band = app.band(&owner, "Rep Band").await;

    let detail = app.get(&format!("/bands/{band}"), &owner).await;
    let repertoire = detail.body["repertoire_id"].as_str().unwrap().to_string();
    assert!(detail.body["open_suggestions"] == 0);
    assert!(detail.body["suggestion_auto_accept_votes"].is_null());

    // A setlist called "Repertoire" is still allowed (the repertoire
    // doesn't count for uniqueness), and the repertoire lists first.
    let other = app.setlist(&owner, "Repertoire", Some(&band)).await;
    let setlists = app.get(&format!("/bands/{band}/setlists"), &owner).await;
    assert_eq!(setlists.body["data"][0]["id"], repertoire.as_str());
    assert_eq!(setlists.body["data"][0]["is_repertoire"], true);
    assert_eq!(setlists.body["data"][1]["is_repertoire"], false);

    // Adding a song to any band setlist adds it to the repertoire too.
    let song = app.song_id(&owner, "Autor", "Sucesso").await;
    app.add_to_setlist(&owner, &other, &song).await;
    assert_eq!(
        app.setlist_titles(&owner, &repertoire).await,
        vec!["Sucesso"]
    );
    let picker = app
        .get(&format!("/bands/{band}/repertoire?q=suc"), &owner)
        .await;
    assert_eq!(picker.status, StatusCode::OK, "{}", picker.body);
    assert_eq!(picker.body["meta"]["total_items"], 1);
    assert!(picker.body["data"][0]["band_id"].is_string());
    let none = app
        .get(&format!("/bands/{band}/repertoire?q=zzz"), &owner)
        .await;
    assert_eq!(none.body["meta"]["total_items"], 0);

    // Can't be renamed or deleted; its description can change.
    let renamed = app
        .patch(
            &format!("/setlists/{repertoire}"),
            &owner,
            json!({ "title": "X" }),
        )
        .await;
    assert_eq!(renamed.code(), "REPERTOIRE_PROTECTED");
    let described = app
        .patch(
            &format!("/setlists/{repertoire}"),
            &owner,
            json!({ "description": "Tudo que tocamos" }),
        )
        .await;
    assert_eq!(described.status, StatusCode::OK);
    assert_eq!(
        app.delete(&format!("/setlists/{repertoire}"), &owner)
            .await
            .code(),
        "REPERTOIRE_PROTECTED"
    );
    // Duplicating it into a personal setlist is fine.
    let copy = app
        .post(
            &format!("/setlists/{repertoire}/duplicate"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(copy.status, StatusCode::CREATED, "{}", copy.body);
    assert_eq!(copy.body["is_repertoire"], false);
}

async fn setup_suggestions(app: &TestApp) -> (String, String, String, String, String) {
    let (_, owner) = app.user("sugowner", Role::User).await;
    let (_, member) = app.user("sugmember", Role::User).await;
    let band = app.band(&owner, "Suggest Band").await;
    app.join_band(&owner, &member, &band, None).await;
    let song = app.song_id(&member, "Compositor", "Ideia").await;
    (owner, member, band, song, String::new())
}

#[tokio::test]
async fn members_suggest_songs_vote_and_managers_decide() {
    let app = app!();
    let (owner, member, band, song, _) = setup_suggestions(&app).await;

    let created = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song, "note": "Para o show de sábado" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    assert_eq!(created.body["status"], "open");
    assert_eq!(created.body["votes"]["up"], 1);
    assert_eq!(created.body["my_vote"], 1);
    assert_eq!(created.body["setlist"]["is_repertoire"], true);
    assert_eq!(created.body["song"]["title"], "Ideia");
    assert_eq!(created.body["suggested_by"]["username"], "sugmember");
    assert_eq!(created.body["song_title"], "Ideia");
    assert_eq!(created.body["artist_name"], "Compositor");

    // The owner was told; the suggester wasn't.
    let owner_notes = app.get("/notifications", &owner).await;
    assert!(
        owner_notes
            .body
            .to_string()
            .contains("band_suggestion_created")
    );
    assert!(owner_notes.body.to_string().contains("sugmember"));
    assert!(
        !app.get("/notifications", &member)
            .await
            .body
            .to_string()
            .contains("band_suggestion_created")
    );

    let duplicate = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(duplicate.code(), "ALREADY_EXISTS");

    let vote = app
        .put(
            &format!("/bands/{band}/suggestions/{id}/vote"),
            &owner,
            json!({ "value": -1 }),
        )
        .await;
    assert_eq!(vote.status, StatusCode::OK, "{}", vote.body);
    assert_eq!(vote.body["votes"]["down"], 1);
    assert_eq!(vote.body["my_vote"], -1);
    let unvote = app
        .delete(&format!("/bands/{band}/suggestions/{id}/vote"), &owner)
        .await;
    assert_eq!(unvote.body["votes"]["down"], 0);
    let bad_vote = app
        .put(
            &format!("/bands/{band}/suggestions/{id}/vote"),
            &owner,
            json!({ "value": 5 }),
        )
        .await;
    assert_eq!(bad_vote.code(), "VALIDATION_ERROR");

    // Members can't accept (no manage_setlists by default), nor withdraw
    // someone else's suggestion.
    let denied = app
        .post(
            &format!("/bands/{band}/suggestions/{id}/accept"),
            &member,
            json!({}),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let not_yours = app
        .post(
            &format!("/bands/{band}/suggestions/{id}/withdraw"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(not_yours.status, StatusCode::FORBIDDEN);

    let accepted = app
        .post(
            &format!("/bands/{band}/suggestions/{id}/accept"),
            &owner,
            json!({ "note": "Boa" }),
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    assert_eq!(accepted.body["status"], "accepted");
    assert_eq!(accepted.body["resolved_by_username"], "sugowner");
    assert_eq!(accepted.body["resolution_note"], "Boa");

    // A band copy is now in the repertoire.
    let detail = app.get(&format!("/bands/{band}"), &owner).await;
    let repertoire = detail.body["repertoire_id"].as_str().unwrap();
    assert_eq!(app.setlist_titles(&owner, repertoire).await, vec!["Ideia"]);
    assert_eq!(detail.body["open_suggestions"], 0);
    let notes = app.get("/notifications", &member).await;
    assert!(notes.body.to_string().contains("band_suggestion_resolved"));

    // Closed: no more votes, and the song is already there.
    let closed = app
        .put(
            &format!("/bands/{band}/suggestions/{id}/vote"),
            &member,
            json!({ "value": 1 }),
        )
        .await;
    assert_eq!(closed.code(), "SUGGESTION_CLOSED");
    let again = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(again.code(), "SONG_ALREADY_IN_SETLIST");

    let open = app
        .get(&format!("/bands/{band}/suggestions"), &member)
        .await;
    assert_eq!(open.body["meta"]["total_items"], 0);
    let all = app
        .get(&format!("/bands/{band}/suggestions?status=all"), &member)
        .await;
    assert_eq!(all.body["meta"]["total_items"], 1);
}

#[tokio::test]
async fn suggestions_are_rejected_withdrawn_or_accepted_by_votes() {
    let app = app!();
    let (owner, member, band, song, _) = setup_suggestions(&app).await;
    let (_, third) = app.user("sugthird", Role::User).await;
    app.join_band(&owner, &third, &band, None).await;
    let (_, outsider) = app.user("sugoutsider", Role::User).await;

    // Only the caller's own songs (or the band's) can be suggested.
    let foreign = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &third,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get(&format!("/bands/{band}/suggestions"), &outsider)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let first = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    let first_id = first.body["id"].as_str().unwrap().to_string();
    let rejected = app
        .post(
            &format!("/bands/{band}/suggestions/{first_id}/reject"),
            &owner,
            json!({ "note": "Agora não" }),
        )
        .await;
    assert_eq!(rejected.body["status"], "rejected");

    let second = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    let second_id = second.body["id"].as_str().unwrap().to_string();
    let withdrawn = app
        .post(
            &format!("/bands/{band}/suggestions/{second_id}/withdraw"),
            &member,
            json!({}),
        )
        .await;
    assert_eq!(withdrawn.body["status"], "withdrawn");

    // With a threshold of 1, one up-vote from another member accepts it
    // automatically; the suggester's own vote never counts.
    let threshold = app
        .patch(
            &format!("/bands/{band}"),
            &owner,
            json!({ "suggestion_auto_accept_votes": 1 }),
        )
        .await;
    assert_eq!(threshold.status, StatusCode::OK, "{}", threshold.body);
    let third_try = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    let third_id = third_try.body["id"].as_str().unwrap().to_string();
    assert_eq!(third_try.body["status"], "open");
    let voted = app
        .put(
            &format!("/bands/{band}/suggestions/{third_id}/vote"),
            &third,
            json!({ "value": 1 }),
        )
        .await;
    assert_eq!(voted.status, StatusCode::OK, "{}", voted.body);
    assert_eq!(voted.body["status"], "accepted");
    assert!(voted.body["resolved_by_username"].is_null());
    let repertoire = app.get(&format!("/bands/{band}"), &owner).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(app.setlist_titles(&owner, &repertoire).await, vec!["Ideia"]);

    let out_of_range = app
        .patch(
            &format!("/bands/{band}"),
            &owner,
            json!({ "suggestion_auto_accept_votes": 101 }),
        )
        .await;
    assert_eq!(out_of_range.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn suggestions_need_the_plan_feature() {
    let app = app!();
    let (_owner, member, band, song, _) = setup_suggestions(&app).await;
    app.enforce_billing().await;
    // A plan without suggestions.
    sqlx::query(
        "UPDATE plans SET features = features || '{\"song_suggestions\": false}' WHERE code = 'basic'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let (_, admin) = app.user("sugadmin", Role::Admin).await;
    let member_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'sugmember'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let granted = app
        .put(
            &format!("/admin/users/{member_id}/subscription"),
            &admin,
            json!({ "plan_code": "basic", "days": 30 }),
        )
        .await;
    assert_eq!(granted.status, StatusCode::OK, "{}", granted.body);
    let gated = app
        .post(
            &format!("/bands/{band}/suggestions"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(gated.code(), "FEATURE_NOT_IN_PLAN");
    assert_eq!(gated.body["meta"]["feature"], "song_suggestions");
}

#[tokio::test]
async fn band_reminders_follow_authorship_and_roles() {
    let app = app!();
    let (_, owner) = app.user("noteowner", Role::User).await;
    let (_, member) = app.user("notemember", Role::User).await;
    let (_, other) = app.user("noteother", Role::User).await;
    let band = app.band(&owner, "Note Band").await;
    app.join_band(&owner, &member, &band, None).await;
    app.join_band(&owner, &other, &band, None).await;

    let created = app
        .post(
            &format!("/bands/{band}/notes"),
            &member,
            json!({ "content": "  Ensaio quinta 19h  ", "color": "yellow", "due_at": "2030-01-01T19:00:00" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["content"], "Ensaio quinta 19h");
    assert_eq!(created.body["author"]["username"], "notemember");
    let note = created.body["id"].as_str().unwrap().to_string();

    let pin_denied = app
        .post(
            &format!("/bands/{band}/notes"),
            &member,
            json!({ "content": "Fixe isto", "is_pinned": true }),
        )
        .await;
    assert_eq!(pin_denied.status, StatusCode::FORBIDDEN);
    let pinned = app
        .post(
            &format!("/bands/{band}/notes"),
            &owner,
            json!({ "content": "Importante", "is_pinned": true }),
        )
        .await;
    assert_eq!(pinned.status, StatusCode::CREATED);

    let list = app.get(&format!("/bands/{band}/notes"), &other).await;
    let notes = list.body.as_array().unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["content"], "Importante");
    assert_eq!(notes[0]["can_edit"], false);
    assert_eq!(notes[1]["can_edit"], false);
    let as_author = app.get(&format!("/bands/{band}/notes"), &member).await;
    assert_eq!(as_author.body[1]["can_edit"], true);

    let foreign_edit = app
        .patch(
            &format!("/bands/{band}/notes/{note}"),
            &other,
            json!({ "content": "x" }),
        )
        .await;
    assert_eq!(foreign_edit.status, StatusCode::FORBIDDEN);
    let own_edit = app
        .patch(
            &format!("/bands/{band}/notes/{note}"),
            &member,
            json!({ "content": "Ensaio sexta", "due_at": null }),
        )
        .await;
    assert_eq!(own_edit.status, StatusCode::OK, "{}", own_edit.body);
    assert!(own_edit.body["due_at"].is_null());
    let own_pin = app
        .patch(
            &format!("/bands/{band}/notes/{note}"),
            &member,
            json!({ "is_pinned": true }),
        )
        .await;
    assert_eq!(own_pin.status, StatusCode::FORBIDDEN);
    let empty = app
        .post(
            &format!("/bands/{band}/notes"),
            &member,
            json!({ "content": "   " }),
        )
        .await;
    assert_eq!(empty.code(), "VALIDATION_ERROR");

    assert_eq!(
        app.delete(&format!("/bands/{band}/notes/{note}"), &other)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.delete(&format!("/bands/{band}/notes/{note}"), &owner)
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    // At most 100 per band.
    sqlx::query(
        "INSERT INTO band_notes (id, band_id, author_id, content, created_at, updated_at)
         SELECT gen_random_uuid(), $1::uuid, NULL, 'n', NOW(), NOW() FROM generate_series(1, 99)",
    )
    .bind(&band)
    .execute(&app.pool)
    .await
    .unwrap();
    let full = app
        .post(
            &format!("/bands/{band}/notes"),
            &member,
            json!({ "content": "101" }),
        )
        .await;
    assert_eq!(full.code(), "QUOTA_EXCEEDED");
    assert_eq!(full.body["meta"]["resource"], "band_notes");
    assert_eq!(full.body["meta"]["limit"], 100);
}

#[tokio::test]
async fn admins_cannot_hand_out_their_own_role() {
    let app = app!();
    let (_, owner) = app.user("topdog", Role::User).await;
    let (admin_id, admin) = app.user("seconddog", Role::User).await;
    let (member_id, member) = app.user("thirddog", Role::User).await;
    let band = app.band(&owner, "Hierarchy").await;
    app.join_band(&owner, &admin, &band, None).await;
    app.join_band(&owner, &member, &band, None).await;

    let promoted = app
        .patch(
            &format!("/bands/{band}/members/{admin_id}"),
            &owner,
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let escalation = app
        .patch(
            &format!("/bands/{band}/members/{member_id}"),
            &admin,
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(escalation.status, StatusCode::FORBIDDEN);
    let allowed = app
        .patch(
            &format!("/bands/{band}/members/{member_id}"),
            &admin,
            json!({ "role": "moderator" }),
        )
        .await;
    assert_eq!(allowed.status, StatusCode::OK);
    let notes = app.get("/notifications", &member).await;
    assert!(notes.body.to_string().contains("band_role_changed"));
}

#[tokio::test]
async fn invites_are_long_expire_and_respect_limits_atomically() {
    let app = app!();
    let (owner_id, owner) = app.user("inviter", Role::User).await;
    let (_, joiner) = app.user("joiner", Role::User).await;
    let band = app.band(&owner, "Invite Band").await;

    let invite = app
        .post(&format!("/bands/{band}/invites"), &owner, json!({}))
        .await;
    let code = invite.body["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 16);
    let expires: chrono::NaiveDateTime =
        serde_json::from_value(invite.body["expires_at"].clone()).unwrap();
    let days = (expires - chrono::Utc::now().naive_utc()).num_hours();
    assert!((167..=168).contains(&days), "{days}");

    // The band is full (owner's limit: one member, the owner).
    app.set_quota(
        owner_id,
        QuotaOverrides {
            band_members: Some(1),
            ..Default::default()
        },
    )
    .await;
    let full = app
        .post(&format!("/invites/{code}/accept"), &joiner, json!({}))
        .await;
    assert_eq!(full.code(), "QUOTA_EXCEEDED", "{}", full.body);
    assert_eq!(full.body["meta"]["resource"], "band_members");
    // The use wasn't consumed.
    let uses: i32 = sqlx::query_scalar("SELECT uses_count FROM band_invites WHERE code = $1")
        .bind(&code)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(uses, 0);

    // Guessing is throttled per client IP.
    let mut throttled = false;
    for _ in 0..15 {
        let attempt = app
            .request_with_headers(
                Method::POST,
                "/invites/AAAAAAAAAAAAAAAA/accept",
                Some(&joiner),
                Some(json!({})),
                &[("x-forwarded-for", "203.0.113.9")],
            )
            .await;
        if attempt.status == StatusCode::TOO_MANY_REQUESTS {
            throttled = true;
            break;
        }
        assert_eq!(attempt.code(), "INVITE_INVALID");
    }
    assert!(throttled);
}

#[tokio::test]
async fn band_responses_carry_the_callers_effective_permissions() {
    let app = app!();
    let (_, owner) = app.user("permowner", Role::User).await;
    let (_, member) = app.user("permmember", Role::User).await;
    let (_, moderator) = app.user("permmod", Role::User).await;
    let band = app.band(&owner, "Perm Band").await;
    app.join_band(&owner, &member, &band, None).await;
    app.join_band(&owner, &moderator, &band, Some("moderator"))
        .await;
    let setlist = app.setlist(&owner, "Show", Some(&band)).await;

    let perms = |token: String| {
        let app = &app;
        let band = band.clone();
        async move {
            let detail = app.get(&format!("/bands/{band}"), &token).await;
            assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
            let list = app.get("/bands", &token).await;
            let listed = list
                .body
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["id"] == band.as_str())
                .unwrap()
                .clone();
            assert_eq!(listed["my_permissions"], detail.body["my_permissions"]);
            detail.body["my_permissions"].clone()
        }
    };

    let all = json!({ "manage_setlists": true, "manage_songs": true, "export_pdf": true });
    assert_eq!(perms(owner.clone()).await, all);
    assert_eq!(perms(moderator.clone()).await, all);
    assert_eq!(
        perms(member.clone()).await,
        json!({ "manage_setlists": false, "manage_songs": false, "export_pdf": true })
    );
    // What the API enforces matches.
    let denied = app
        .patch(
            &format!("/setlists/{setlist}"),
            &member,
            json!({ "description": "x" }),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let updated = app
        .put(
            &format!("/bands/{band}/permissions"),
            &owner,
            json!({ "permissions": [
                { "role": "member", "permission": "manage_setlists", "allowed": true },
                { "role": "member", "permission": "export_pdf", "allowed": false },
                { "role": "moderator", "permission": "manage_songs", "allowed": false }
            ] }),
        )
        .await;
    assert!(updated.status.is_success(), "{}", updated.body);
    assert_eq!(
        perms(member.clone()).await,
        json!({ "manage_setlists": true, "manage_songs": false, "export_pdf": false })
    );
    assert_eq!(
        perms(moderator.clone()).await,
        json!({ "manage_setlists": true, "manage_songs": false, "export_pdf": true })
    );
    let allowed = app
        .patch(
            &format!("/setlists/{setlist}"),
            &member,
            json!({ "description": "x" }),
        )
        .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accept_and_reject_resolve_a_suggestion_exactly_once() {
    let app = app!();
    let (owner, member, band, _, _) = setup_suggestions(&app).await;
    let repertoire = app.get(&format!("/bands/{band}"), &owner).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();

    for round in 0..4 {
        let song = app
            .song_id(&member, "Compositor", &format!("Corrida {round}"))
            .await;
        let created = app
            .post(
                &format!("/bands/{band}/suggestions"),
                &member,
                json!({ "song_id": song }),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        let id = created.body["id"].as_str().unwrap().to_string();

        let responses = app
            .concurrent(
                Some(&owner),
                vec![
                    (
                        Method::POST,
                        format!("/bands/{band}/suggestions/{id}/accept"),
                        json!({}),
                    ),
                    (
                        Method::POST,
                        format!("/bands/{band}/suggestions/{id}/reject"),
                        json!({}),
                    ),
                    (
                        Method::POST,
                        format!("/bands/{band}/suggestions/{id}/accept"),
                        json!({}),
                    ),
                ],
            )
            .await;
        let winners: Vec<_> = responses
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .collect();
        assert_eq!(winners.len(), 1, "exactly one resolution wins");
        for loser in responses.iter().filter(|r| r.status != StatusCode::OK) {
            assert_eq!(loser.code(), "SUGGESTION_CLOSED", "{}", loser.body);
        }

        let status = winners[0].body["status"].as_str().unwrap().to_string();
        let titles = app.setlist_titles(&owner, &repertoire).await;
        let title = format!("Corrida {round}");
        let copies = titles.iter().filter(|t| **t == title).count();
        match status.as_str() {
            "accepted" => assert_eq!(copies, 1, "{titles:?}"),
            "rejected" => assert_eq!(copies, 0, "{titles:?}"),
            other => panic!("unexpected status {other}"),
        }
    }

    // Two suggestions of the same song (for two setlists) accepted at once
    // share one band copy.
    let song = app.song_id(&member, "Outro Autor", "Dupla").await;
    let other_setlist = app.setlist(&owner, "Show de Sexta", Some(&band)).await;
    let mut ids = Vec::new();
    for setlist in [&repertoire, &other_setlist] {
        let created = app
            .post(
                &format!("/bands/{band}/suggestions"),
                &member,
                json!({ "song_id": song, "setlist_id": setlist }),
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        ids.push(created.body["id"].as_str().unwrap().to_string());
    }
    let responses = app
        .concurrent(
            Some(&owner),
            ids.iter()
                .map(|id| {
                    (
                        Method::POST,
                        format!("/bands/{band}/suggestions/{id}/accept"),
                        json!({}),
                    )
                })
                .collect(),
        )
        .await;
    for r in &responses {
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }
    let copies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM songs WHERE band_id = $1::uuid AND forked_from = $2::uuid AND deleted_at IS NULL",
    )
    .bind(&band)
    .bind(&song)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(copies, 1);
    let artists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM artists WHERE band_id = $1::uuid AND name = 'Outro Autor' AND deleted_at IS NULL",
    )
    .bind(&band)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(artists, 1);
    assert!(
        app.setlist_titles(&owner, &other_setlist)
            .await
            .contains(&"Dupla".to_string())
    );
}
