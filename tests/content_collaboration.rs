//! Setlist collaborators: sharing a personal setlist without a band, and
//! who added each song.

mod common;

use axum::http::StatusCode;
use common::TestApp;
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

/// Invites `username` to `setlist` as `role` and has them accept.
async fn collaborate(
    app: &TestApp,
    owner: &str,
    guest: &str,
    username: &str,
    setlist: &str,
    role: &str,
) {
    let invited = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            owner,
            json!({ "username": username, "role": role }),
        )
        .await;
    assert_eq!(invited.status, StatusCode::NO_CONTENT, "{}", invited.body);
    let accepted = app
        .post(
            &format!("/setlists/{setlist}/invitation/accept"),
            guest,
            json!({}),
        )
        .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT, "{}", accepted.body);
}

#[tokio::test]
async fn an_invite_grants_access_only_once_accepted() {
    let app = app!();
    let (_, owner) = app.user("dono", Role::User).await;
    let (guest_id, guest) = app.user("cantor", Role::User).await;
    let setlist = app.setlist(&owner, "Show único", None).await;

    let invited = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &owner,
            json!({ "username": "CANTOR" }),
        )
        .await;
    assert_eq!(invited.status, StatusCode::NO_CONTENT, "{}", invited.body);

    // Pending: no access yet, but the invite is listed and notified.
    let hidden = app.get(&format!("/setlists/{setlist}"), &guest).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let invitations = app.get("/setlists/invitations", &guest).await;
    assert_eq!(invitations.body.as_array().unwrap().len(), 1);
    assert_eq!(invitations.body[0]["setlist_title"], "Show único");
    assert_eq!(invitations.body[0]["owner_username"], "dono");
    assert_eq!(invitations.body[0]["role"], "editor");
    let notifications = app.get("/notifications", &guest).await;
    assert!(
        notifications
            .body
            .to_string()
            .contains("setlist_invitation"),
        "{}",
        notifications.body
    );

    // Inviting again is refused.
    let again = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &owner,
            json!({ "username": "cantor" }),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "ALREADY_MEMBER");

    let accepted = app
        .post(
            &format!("/setlists/{setlist}/invitation/accept"),
            &guest,
            json!({}),
        )
        .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT);

    let seen = app.get(&format!("/setlists/{setlist}"), &guest).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert_eq!(seen.body["collaborator_role"], "editor");
    assert_eq!(seen.body["collaborator_count"], 1);

    let shared = app.get("/setlists/shared", &guest).await;
    assert_eq!(shared.body["meta"]["total_items"], 1);
    assert_eq!(shared.body["data"][0]["id"], setlist.as_str());
    // Not one of the guest's own setlists.
    let own = app.get("/setlists", &guest).await;
    assert_eq!(own.body["meta"]["total_items"], 0);

    let list = app
        .get(&format!("/setlists/{setlist}/collaborators"), &guest)
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(list.body["owner"]["username"], "dono");
    assert_eq!(
        list.body["collaborators"][0]["user_id"],
        guest_id.to_string()
    );
    assert_eq!(list.body["collaborators"][0]["accepted"], true);
}

#[tokio::test]
async fn unknown_or_self_invites_are_refused() {
    let app = app!();
    let (_, owner) = app.user("dona", Role::User).await;
    let (_, stranger) = app.user("estranho", Role::User).await;
    let setlist = app.setlist(&owner, "Sexta", None).await;

    let unknown = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &owner,
            json!({ "username": "ninguem" }),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.code(), "USER_NOT_FOUND");

    let myself = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &owner,
            json!({ "username": "dona" }),
        )
        .await;
    assert_eq!(myself.status, StatusCode::CONFLICT);

    // Someone who can't see the setlist learns nothing.
    let probe = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &stranger,
            json!({ "username": "dona" }),
        )
        .await;
    assert_eq!(probe.status, StatusCode::NOT_FOUND);
    assert_eq!(probe.code(), "NOT_FOUND");
}

#[tokio::test]
async fn roles_decide_what_a_collaborator_may_do() {
    let app = app!();
    let (_, owner) = app.user("titular", Role::User).await;
    let (_, viewer) = app.user("leitor", Role::User).await;
    let (_, editor) = app.user("editora", Role::User).await;
    let (manager_id, manager) = app.user("gerente", Role::User).await;
    let setlist = app.setlist(&owner, "Casamento", None).await;
    collaborate(&app, &owner, &viewer, "leitor", &setlist, "viewer").await;
    collaborate(&app, &owner, &editor, "editora", &setlist, "editor").await;
    collaborate(&app, &owner, &manager, "gerente", &setlist, "manager").await;

    // Viewers read, and play it live.
    assert_eq!(
        app.get(&format!("/setlists/{setlist}/items"), &viewer)
            .await
            .status,
        StatusCode::OK
    );
    let viewer_song = app.song_id(&viewer, "Artista", "Do leitor").await;
    let refused = app
        .post(
            &format!("/setlists/{setlist}/songs"),
            &viewer,
            json!({ "song_id": viewer_song }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    // Editors change the running order, not the details.
    let block = app
        .post(
            &format!("/setlists/{setlist}/blocks"),
            &editor,
            json!({ "name": "Entrada" }),
        )
        .await;
    assert_eq!(block.status, StatusCode::CREATED, "{}", block.body);
    let rename = app
        .patch(
            &format!("/setlists/{setlist}"),
            &editor,
            json!({ "title": "Outro" }),
        )
        .await;
    assert_eq!(rename.status, StatusCode::FORBIDDEN);

    // Managers edit details and manage editors, but not other managers.
    let rename = app
        .patch(
            &format!("/setlists/{setlist}"),
            &manager,
            json!({ "title": "Casamento da Ana" }),
        )
        .await;
    assert_eq!(rename.status, StatusCode::OK, "{}", rename.body);
    let promote = app
        .patch(
            &format!("/setlists/{setlist}/collaborators/{manager_id}"),
            &manager,
            json!({ "role": "editor" }),
        )
        .await;
    assert_eq!(promote.status, StatusCode::FORBIDDEN);
    let (_, other) = app.user("outro", Role::User).await;
    let as_manager = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &manager,
            json!({ "username": "outro", "role": "manager" }),
        )
        .await;
    assert_eq!(as_manager.status, StatusCode::FORBIDDEN);
    collaborate(&app, &manager, &other, "outro", &setlist, "viewer").await;

    // Only the owner deletes and shares publicly.
    for token in [&viewer, &editor, &manager] {
        let share = app
            .post(&format!("/setlists/{setlist}/share"), token, json!({}))
            .await;
        assert_eq!(share.status, StatusCode::FORBIDDEN, "{}", share.body);
        let delete = app.delete(&format!("/setlists/{setlist}"), token).await;
        assert_eq!(delete.status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn songs_record_who_added_them_and_stay_when_their_owner_leaves() {
    let app = app!();
    let (owner_id, owner) = app.user("violao", Role::User).await;
    let (guest_id, guest) = app.user("voz", Role::User).await;
    let setlist = app.setlist(&owner, "Bar do Zé", None).await;
    collaborate(&app, &owner, &guest, "voz", &setlist, "editor").await;

    let mine = app.song_id(&owner, "Artista", "Minha").await;
    let theirs = app.song_id(&guest, "Artista", "Dele").await;
    for (token, song) in [(&owner, &mine), (&guest, &theirs)] {
        let added = app
            .post(
                &format!("/setlists/{setlist}/songs"),
                token,
                json!({ "song_id": song }),
            )
            .await;
        assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    }

    // A collaborator can only bring their own songs.
    let other = app
        .post(
            &format!("/setlists/{setlist}/songs"),
            &guest,
            json!({ "song_id": mine }),
        )
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);

    let items = app.get(&format!("/setlists/{setlist}/items"), &owner).await;
    let songs: Vec<_> = items
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["item_type"] == "song")
        .collect();
    assert_eq!(songs.len(), 2, "{}", items.body);
    assert_eq!(songs[0]["song"]["added_by"], owner_id.to_string());
    assert_eq!(songs[1]["song"]["added_by"], guest_id.to_string());
    assert_eq!(songs[1]["song"]["added_by_username"], "voz");
    assert!(songs[1]["song"]["added_at"].is_string());

    let setlist_row = app.get(&format!("/setlists/{setlist}"), &owner).await;
    assert_eq!(setlist_row.body["song_count"], 2);
    assert_eq!(setlist_row.body["updated_by_username"], "voz");

    // Leaving keeps the guest's song in the setlist, held by it: same
    // place, key and attribution, in no one's library.
    let left = app
        .delete(
            &format!("/setlists/{setlist}/collaborators/{guest_id}"),
            &guest,
        )
        .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/setlists/{setlist}"), &guest)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let items = app.get(&format!("/setlists/{setlist}/items"), &owner).await;
    let songs: Vec<_> = items
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["item_type"] == "song")
        .map(|i| i["song"].clone())
        .collect();
    assert_eq!(songs.len(), 2, "{}", items.body);
    assert_eq!(songs[0]["id"], mine.as_str());
    assert!(songs[0].get("held").is_none());
    assert_eq!(songs[1]["title"], "Dele");
    assert_eq!(songs[1]["held"], true);
    assert_eq!(songs[1]["added_by_username"], "voz");
    assert_ne!(songs[1]["id"], theirs.as_str());
    let setlist_row = app.get(&format!("/setlists/{setlist}"), &owner).await;
    assert_eq!(setlist_row.body["song_count"], 2);

    // It went to nobody's library.
    let library = app.get("/songs", &owner).await;
    assert_eq!(library.body["meta"]["total_items"], 1, "{}", library.body);

    // Held songs are reordered, retuned and removed like any other.
    let held_id = songs[1]["id"].as_str().unwrap().to_string();
    let key = app
        .patch(
            &format!("/setlists/{setlist}/songs/{held_id}"),
            &owner,
            json!({ "transpose": -2 }),
        )
        .await;
    assert_eq!(key.status, StatusCode::NO_CONTENT, "{}", key.body);
    let reorder = app
        .patch(
            &format!("/setlists/{setlist}/songs/reorder"),
            &owner,
            json!({ "song_ids": [held_id, mine] }),
        )
        .await;
    assert_eq!(reorder.status, StatusCode::OK, "{}", reorder.body);
    let listed = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(listed.body["data"][0]["id"], held_id.as_str());
    assert_eq!(listed.body["data"][0]["transpose"], -2);

    // Duplicating keeps the held song held (not added to a library).
    let copy = app
        .post(&format!("/setlists/{setlist}/duplicate"), &owner, json!({}))
        .await;
    assert_eq!(copy.status, StatusCode::CREATED, "{}", copy.body);
    assert_eq!(copy.body["song_count"], 2);
    assert_eq!(
        app.get("/songs", &owner).await.body["meta"]["total_items"],
        1
    );

    let removed = app
        .delete(&format!("/setlists/{setlist}/songs/{held_id}"), &owner)
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let after = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(after.body["meta"]["total_items"], 1, "{}", after.body);
}

#[tokio::test]
async fn copying_a_held_song_puts_it_in_the_library_on_request() {
    let app = app!();
    let (_, owner) = app.user("guitarra", Role::User).await;
    let (guest_id, guest) = app.user("cantora", Role::User).await;
    let (_, viewer) = app.user("plateia", Role::User).await;
    let setlist = app.setlist(&owner, "Casamento", None).await;
    collaborate(&app, &owner, &guest, "cantora", &setlist, "editor").await;
    collaborate(&app, &owner, &viewer, "plateia", &setlist, "viewer").await;
    let theirs = app.song_id(&guest, "Artista", "Da cantora").await;
    app.post(
        &format!("/setlists/{setlist}/songs"),
        &guest,
        json!({ "song_id": theirs }),
    )
    .await;

    // While she collaborates, anyone may copy her song for themselves.
    let viewer_copy = app
        .post(
            &format!("/setlists/{setlist}/songs/{theirs}/copy"),
            &viewer,
            json!({}),
        )
        .await;
    assert_eq!(
        viewer_copy.status,
        StatusCode::CREATED,
        "{}",
        viewer_copy.body
    );
    assert_eq!(viewer_copy.body["adopted"], false);
    assert_eq!(
        app.get("/songs", &viewer).await.body["meta"]["total_items"],
        1
    );
    let own = app
        .post(
            &format!("/setlists/{setlist}/songs/{theirs}/copy"),
            &guest,
            json!({}),
        )
        .await;
    assert_eq!(own.status, StatusCode::CONFLICT);
    assert_eq!(own.code(), "SONG_ALREADY_IN_LIBRARY");

    app.delete(
        &format!("/setlists/{setlist}/collaborators/{guest_id}"),
        &owner,
    )
    .await;
    let held = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    let held_id = held.body["data"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(held.body["data"][0]["held"], true);

    // The owner adopts it: it joins their library and the setlist links it.
    let adopted = app
        .post(
            &format!("/setlists/{setlist}/songs/{held_id}/copy"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(adopted.status, StatusCode::CREATED, "{}", adopted.body);
    assert_eq!(adopted.body["adopted"], true);
    let copy_id = adopted.body["song_id"].as_str().unwrap().to_string();
    let songs = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(songs.body["meta"]["total_items"], 1);
    assert_eq!(songs.body["data"][0]["id"], copy_id.as_str());
    assert!(songs.body["data"][0].get("held").is_none());
    assert_eq!(songs.body["data"][0]["added_by_username"], "cantora");
    assert_eq!(
        app.get(&format!("/songs/{copy_id}"), &owner).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn trashing_or_deleting_the_account_keeps_contributed_songs_held() {
    let app = app!();
    let (_, owner) = app.user("teclado", Role::User).await;
    let (guest_id, guest) = app.user("baterista", Role::User).await;
    let setlist = app.setlist(&owner, "Festival", None).await;
    collaborate(&app, &owner, &guest, "baterista", &setlist, "editor").await;
    let first = app.song_id(&guest, "Banda A", "Primeira").await;
    let second = app.song_id(&guest, "Banda B", "Segunda").await;
    for song in [&first, &second] {
        app.post(
            &format!("/setlists/{setlist}/songs"),
            &guest,
            json!({ "song_id": song }),
        )
        .await;
    }

    // Trashing her song: the setlist keeps it.
    let trashed = app.delete(&format!("/songs/{first}"), &guest).await;
    assert_eq!(trashed.status, StatusCode::NO_CONTENT, "{}", trashed.body);
    let songs = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(songs.body["meta"]["total_items"], 2, "{}", songs.body);
    assert_eq!(songs.body["data"][0]["title"], "Primeira");
    assert_eq!(songs.body["data"][0]["held"], true);

    // Deleting her account: the setlist keeps the rest too.
    app.state.user_repo.delete(guest_id).await.unwrap();
    let songs = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(songs.body["meta"]["total_items"], 2, "{}", songs.body);
    assert_eq!(songs.body["data"][1]["title"], "Segunda");
    assert_eq!(songs.body["data"][1]["held"], true);

    // Deleting the setlist takes its held songs with it.
    let setlist_uuid: uuid::Uuid = setlist.parse().unwrap();
    app.state.setlist_repo.delete(setlist_uuid).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM setlist_held_songs")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn band_setlists_record_who_added_songs_but_take_no_collaborators() {
    let app = app!();
    let (_, owner) = app.user("lider", Role::User).await;
    let (member_id, member) = app.user("baixista", Role::User).await;
    let band = app.band(&owner, "Os Tais").await;
    app.join_band(&owner, &member, &band, Some("admin")).await;
    let setlist = app.setlist(&owner, "Turnê", Some(&band)).await;

    let song = app.song_id(&member, "Artista", "Do baixista").await;
    let added = app
        .post(
            &format!("/setlists/{setlist}/songs"),
            &member,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    let songs = app.get(&format!("/setlists/{setlist}/songs"), &owner).await;
    assert_eq!(songs.body["data"][0]["added_by"], member_id.to_string());
    assert_eq!(songs.body["data"][0]["added_by_username"], "baixista");

    let invite = app
        .post(
            &format!("/setlists/{setlist}/collaborators"),
            &owner,
            json!({ "username": "baixista" }),
        )
        .await;
    assert_eq!(invite.status, StatusCode::CONFLICT);
    assert_eq!(invite.code(), "COLLABORATION_UNAVAILABLE");
}

#[tokio::test]
async fn declined_and_removed_invites_leave_no_access() {
    let app = app!();
    let (_, owner) = app.user("anfitria", Role::User).await;
    let (guest_id, guest) = app.user("convidado", Role::User).await;
    let setlist = app.setlist(&owner, "Aniversário", None).await;

    app.post(
        &format!("/setlists/{setlist}/collaborators"),
        &owner,
        json!({ "username": "convidado", "role": "viewer" }),
    )
    .await;
    let declined = app
        .delete(&format!("/setlists/{setlist}/invitation"), &guest)
        .await;
    assert_eq!(declined.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/setlists/invitations", &guest)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        0
    );

    collaborate(&app, &owner, &guest, "convidado", &setlist, "viewer").await;
    let removed = app
        .delete(
            &format!("/setlists/{setlist}/collaborators/{guest_id}"),
            &owner,
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&format!("/setlists/{setlist}"), &guest)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let setlist_row = app.get(&format!("/setlists/{setlist}"), &owner).await;
    assert_eq!(setlist_row.body["collaborator_count"], 0);
}
