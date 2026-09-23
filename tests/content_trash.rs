//! The trash: soft delete, restore, permanent delete, purge, and every read
//! path ignoring trashed rows.

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
async fn a_trashed_song_vanishes_everywhere_and_comes_back_on_restore() {
    let app = app!();
    let (_, user) = app.user("trasher", Role::User).await;
    let keep = app.song_id(&user, "Artista", "Fica").await;
    let gone = app.song_id(&user, "Artista", "Vai").await;
    let setlist = app.setlist(&user, "Show", None).await;
    app.add_to_setlist(&user, &setlist, &keep).await;
    app.add_to_setlist(&user, &setlist, &gone).await;
    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    let token = shared.body["share_token"].as_str().unwrap().to_string();

    let deleted = app.delete(&format!("/songs/{gone}"), &user).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);

    // Lists, lookups, setlists, public pages, metrics, exports.
    let songs = app.get("/songs", &user).await;
    assert_eq!(songs.body["meta"]["total_items"], 1);
    assert_eq!(
        app.get(&format!("/songs/{gone}"), &user).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.setlist_titles(&user, &setlist).await, vec!["Fica"]);
    let detail = app.get(&format!("/setlists/{setlist}"), &user).await;
    assert_eq!(detail.body["song_count"], 1);
    let public = app
        .request(
            Method::GET,
            &format!("/public/setlists/{token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.body["songs"].as_array().unwrap().len(), 1);
    let metrics = app.get("/metrics", &user).await;
    assert_eq!(metrics.body["total_songs"], 1, "{}", metrics.body);
    let chordpro = app.get("/songs/export/chordpro", &user).await;
    let text = String::from_utf8(chordpro.bytes.clone()).unwrap();
    assert!(text.contains("Fica") && !text.contains("{title: Vai}"));
    let backup = app.get("/backup/export", &user).await;
    assert_eq!(backup.body["songs"].as_array().unwrap().len(), 1);
    assert_eq!(
        backup.body["setlists"][0]["songs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Updating or deleting it again is not possible either.
    assert_eq!(
        app.patch(&format!("/songs/{gone}"), &user, json!({ "tempo": 90 }))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // The trash lists it, with who deleted it and when it goes away.
    let trash = app.get("/trash", &user).await;
    assert_eq!(trash.status, StatusCode::OK, "{}", trash.body);
    assert_eq!(trash.body["meta"]["total_items"], 1);
    let item = &trash.body["data"][0];
    assert_eq!(item["type"], "song");
    assert_eq!(item["title"], "Vai");
    assert_eq!(item["subtitle"], "Artista");
    assert_eq!(item["deleted_by_username"], "trasher");
    assert!(item["purge_at"].is_string());

    let restored = app
        .post(&format!("/trash/song/{gone}/restore"), &user, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    assert_eq!(
        app.setlist_titles(&user, &setlist).await,
        vec!["Fica", "Vai"]
    );
    let public = app
        .request(
            Method::GET,
            &format!("/public/setlists/{token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.body["songs"].as_array().unwrap().len(), 2);
    assert_eq!(
        app.get("/trash", &user).await.body["meta"]["total_items"],
        0
    );
    let again = app
        .post(&format!("/trash/song/{gone}/restore"), &user, json!({}))
        .await;
    assert_eq!(again.code(), "NOT_IN_TRASH");
}

#[tokio::test]
async fn restoring_checks_names_and_quotas_again() {
    let app = app!();
    let (user_id, user) = app.user("restorer", Role::User).await;
    let song = app.song_id(&user, "Banda", "Mesma").await;
    app.delete(&format!("/songs/{song}"), &user).await;

    // A trashed song doesn't block creating the same title again...
    let replacement = app.song_id(&user, "Banda", "Mesma").await;
    // ...but then it can't come back under the same name.
    let conflict = app
        .post(&format!("/trash/song/{song}/restore"), &user, json!({}))
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.code(), "RESTORE_CONFLICT");
    assert_eq!(conflict.body["meta"]["type"], "song");

    app.delete(&format!("/songs/{replacement}"), &user).await;
    app.song_id(&user, "Banda", "Outra").await;
    app.set_quota(
        user_id,
        QuotaOverrides {
            songs: Some(1),
            ..Default::default()
        },
    )
    .await;
    let over = app
        .post(&format!("/trash/song/{song}/restore"), &user, json!({}))
        .await;
    assert_eq!(over.code(), "QUOTA_EXCEEDED");
    assert_eq!(over.body["meta"]["resource"], "songs");
}

#[tokio::test]
async fn an_artist_goes_to_the_trash_with_its_songs_and_comes_back_with_them() {
    let app = app!();
    let (_, user) = app.user("batcher", Role::User).await;
    let first = app.song_id(&user, "Grupo", "Um").await;
    app.song_id(&user, "Grupo", "Dois").await;
    let artists = app.get("/artists", &user).await;
    let artist = artists.body["data"][0]["id"].as_str().unwrap().to_string();

    // A song trashed before its artist is its own entry.
    app.delete(&format!("/songs/{first}"), &user).await;
    assert_eq!(
        app.delete(&format!("/artists/{artist}"), &user)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get("/artists", &user).await.body["meta"]["total_items"],
        0
    );
    assert_eq!(
        app.get("/songs", &user).await.body["meta"]["total_items"],
        0
    );

    let trash = app.get("/trash", &user).await;
    assert_eq!(trash.body["meta"]["total_items"], 2, "{}", trash.body);
    let artist_item = trash.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "artist")
        .unwrap()
        .clone();
    assert_eq!(artist_item["batch_count"], 1);
    let only_songs = app.get("/trash?type=song", &user).await;
    assert_eq!(only_songs.body["meta"]["total_items"], 1);

    // Restoring the older song alone brings its artist back (just the
    // artist row).
    let restored = app
        .post(&format!("/trash/song/{first}/restore"), &user, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    let songs = app.get("/songs", &user).await;
    assert_eq!(songs.body["meta"]["total_items"], 1);
    assert_eq!(songs.body["data"][0]["title"], "Um");
    assert_eq!(
        app.get("/artists", &user).await.body["meta"]["total_items"],
        1
    );
    // "Dois" is still in the trash, as its own entry now.
    let trash = app.get("/trash", &user).await;
    assert_eq!(trash.body["meta"]["total_items"], 1, "{}", trash.body);
    assert_eq!(trash.body["data"][0]["title"], "Dois");
}

#[tokio::test]
async fn restoring_an_artist_restores_its_batch() {
    let app = app!();
    let (_, user) = app.user("batch2", Role::User).await;
    app.song_id(&user, "Coral", "A").await;
    app.song_id(&user, "Coral", "B").await;
    let artist = app.get("/artists", &user).await.body["data"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.delete(&format!("/artists/{artist}"), &user).await;
    let restored = app
        .post(&format!("/trash/artist/{artist}/restore"), &user, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    assert_eq!(
        app.get("/songs", &user).await.body["meta"]["total_items"],
        2
    );
    let artists = app.get("/artists", &user).await;
    assert_eq!(artists.body["data"][0]["song_count"], 2);
}

#[tokio::test]
async fn restoring_one_song_of_a_trashed_artist_restores_only_that_song() {
    let app = app!();
    let (_, user) = app.user("batch3", Role::User).await;
    let a = app.song_id(&user, "Quarteto", "A").await;
    let b = app.song_id(&user, "Quarteto", "B").await;
    app.song_id(&user, "Quarteto", "C").await;
    let artist = app.get("/artists", &user).await.body["data"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.delete(&format!("/artists/{artist}"), &user).await;

    // B went to the trash with its artist (same batch), but restoring B
    // must not bring A and C back.
    let restored = app
        .post(&format!("/trash/song/{b}/restore"), &user, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    let songs = app.get("/songs", &user).await;
    assert_eq!(songs.body["meta"]["total_items"], 1, "{}", songs.body);
    assert_eq!(songs.body["data"][0]["title"], "B");
    let artists = app.get("/artists", &user).await;
    assert_eq!(artists.body["meta"]["total_items"], 1);
    assert_eq!(artists.body["data"][0]["song_count"], 1);

    // A and C are now separate entries, still restorable one by one.
    let trash = app.get("/trash?type=song", &user).await;
    assert_eq!(trash.body["meta"]["total_items"], 2, "{}", trash.body);
    let restored = app
        .post(&format!("/trash/song/{a}/restore"), &user, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    assert_eq!(
        app.get("/songs", &user).await.body["meta"]["total_items"],
        2
    );
}

#[tokio::test]
async fn trashed_setlists_leave_gigs_and_lists_and_the_repertoire_is_protected() {
    let app = app!();
    let (_, owner) = app.user("gigger", Role::User).await;
    let setlist = app.setlist(&owner, "Noite", None).await;
    let gig = app
        .post(
            "/gigs",
            &owner,
            json!({ "venue": "Bar", "scheduled_at": "2030-05-01T21:00:00", "setlist_id": setlist }),
        )
        .await;
    let gig_id = gig.body["id"].as_str().unwrap().to_string();

    app.delete(&format!("/setlists/{setlist}"), &owner).await;
    assert_eq!(
        app.get("/setlists", &owner).await.body["meta"]["total_items"],
        0
    );
    let detail = app.get(&format!("/gigs/{gig_id}"), &owner).await;
    assert!(detail.body["setlist_id"].is_null(), "{}", detail.body);
    // Its title is free again.
    app.setlist(&owner, "Noite", None).await;
    let conflict = app
        .post(
            &format!("/trash/setlist/{setlist}/restore"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(conflict.code(), "RESTORE_CONFLICT");

    let band = app.band(&owner, "Banda Protegida").await;
    let repertoire = app.get(&format!("/bands/{band}"), &owner).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();
    let protected = app.delete(&format!("/setlists/{repertoire}"), &owner).await;
    assert_eq!(protected.status, StatusCode::CONFLICT);
    assert_eq!(protected.code(), "REPERTOIRE_PROTECTED");

    // Gigs and their restore.
    app.delete(&format!("/gigs/{gig_id}"), &owner).await;
    assert_eq!(
        app.get("/gigs", &owner).await.body["meta"]["total_items"],
        0
    );
    let restored = app
        .post(&format!("/trash/gig/{gig_id}/restore"), &owner, json!({}))
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get("/gigs", &owner).await.body["meta"]["total_items"],
        1
    );
}

#[tokio::test]
async fn permanent_deletes_empty_and_purge() {
    let app = app!();
    let (_, user) = app.user("purger", Role::User).await;
    let (_, stranger) = app.user("stranger", Role::User).await;
    let a = app.song_id(&user, "X", "A").await;
    let b = app.song_id(&user, "X", "B").await;
    let c = app.song_id(&user, "X", "C").await;
    for song in [&a, &b, &c] {
        app.delete(&format!("/songs/{song}"), &user).await;
    }

    // Someone else's trash looks empty.
    let foreign = app
        .post(&format!("/trash/song/{a}/restore"), &stranger, json!({}))
        .await;
    assert_eq!(foreign.code(), "NOT_IN_TRASH");
    assert_eq!(
        app.delete(&format!("/trash/song/{a}"), &stranger)
            .await
            .code(),
        "NOT_IN_TRASH"
    );
    assert_eq!(
        app.get("/trash", &stranger).await.body["meta"]["total_items"],
        0
    );

    assert_eq!(
        app.delete(&format!("/trash/song/{a}"), &user).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.delete(&format!("/trash/song/{a}"), &user).await.code(),
        "NOT_IN_TRASH"
    );
    // A live item isn't in the trash either.
    let live = app.song_id(&user, "X", "Live").await;
    assert_eq!(
        app.delete(&format!("/trash/song/{live}"), &user)
            .await
            .code(),
        "NOT_IN_TRASH"
    );

    // The purge job only takes rows past the retention period.
    sqlx::query(
        "UPDATE songs SET deleted_at = deleted_at - INTERVAL '40 days' WHERE id = $1::uuid",
    )
    .bind(&b)
    .execute(&app.pool)
    .await
    .unwrap();
    let purged = setlyst_api::jobs::trash::purge_trash(&app.pool, 30)
        .await
        .unwrap();
    assert_eq!(purged, 1);
    let trash = app.get("/trash", &user).await;
    assert_eq!(trash.body["meta"]["total_items"], 1);

    let emptied = app
        .request(Method::DELETE, "/trash?scope=personal", Some(&user), None)
        .await;
    assert_eq!(emptied.status, StatusCode::OK, "{}", emptied.body);
    assert_eq!(emptied.body["deleted"], 1);
    assert_eq!(
        app.get("/trash", &user).await.body["meta"]["total_items"],
        0
    );
    // The live song is untouched.
    assert_eq!(
        app.get("/songs", &user).await.body["meta"]["total_items"],
        1
    );
}

#[tokio::test]
async fn band_trash_follows_the_band_permissions() {
    let app = app!();
    let (_, owner) = app.user("leader", Role::User).await;
    let (_, member) = app.user("follower", Role::User).await;
    let band = app.band(&owner, "Trash Band").await;
    app.join_band(&owner, &member, &band, None).await;

    let setlist = app.setlist(&owner, "Set da banda", Some(&band)).await;
    let song = app.song_id(&owner, "Autor", "Da Banda").await;
    app.add_to_setlist(&owner, &setlist, &song).await;
    let band_song = app
        .get(&format!("/setlists/{setlist}/items"), &owner)
        .await
        .body[0]["song"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        app.delete(&format!("/songs/{band_song}"), &owner)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    app.delete(&format!("/setlists/{setlist}"), &owner).await;

    // Members can't manage songs nor setlists by default.
    let denied = app
        .get(&format!("/trash?scope=band&band_id={band}"), &member)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = app
        .post(
            &format!("/trash/setlist/{setlist}/restore"),
            &member,
            json!({}),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let listed = app
        .get(&format!("/trash?scope=band&band_id={band}"), &owner)
        .await;
    assert_eq!(listed.body["meta"]["total_items"], 2, "{}", listed.body);
    assert!(
        listed.body["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["band_name"] == "Trash Band")
    );
    // Not part of the personal trash.
    assert_eq!(
        app.get("/trash", &owner).await.body["meta"]["total_items"],
        0
    );

    let restored = app
        .post(
            &format!("/trash/setlist/{setlist}/restore"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT);
    // The band song is still trashed, so the setlist shows nothing until
    // it is restored too.
    assert!(app.setlist_titles(&owner, &setlist).await.is_empty());
    app.post(
        &format!("/trash/song/{band_song}/restore"),
        &owner,
        json!({}),
    )
    .await;
    assert_eq!(app.setlist_titles(&owner, &setlist).await, vec!["Da Banda"]);
}

#[tokio::test]
async fn staff_views_and_admin_metrics_ignore_trashed_rows() {
    let app = app!();
    let (_, admin) = app.user("overseer", Role::Admin).await;
    let (_, user) = app.user("content.owner", Role::User).await;
    let song = app.song_id(&user, "Y", "Some").await;
    app.song_id(&user, "Y", "Other").await;
    app.delete(&format!("/songs/{song}"), &user).await;

    let listed = app.get("/admin/songs", &admin).await;
    assert_eq!(listed.body["meta"]["total_items"], 1, "{}", listed.body);
    assert_eq!(
        app.get(&format!("/admin/songs/{song}"), &admin)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let metrics = app.get("/metrics", &admin).await;
    assert_eq!(metrics.body["total_songs"], 1, "{}", metrics.body);
}
