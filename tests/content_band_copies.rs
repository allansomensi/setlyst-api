//! A band's copies of its members' songs: the repertoire's trash, and
//! keeping a copy in step with the personal song it came from.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};
use setlyst_api::models::user::Role;

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Adds `song_id` to `setlist_id`; returns the response body.
async fn add(app: &TestApp, token: &str, setlist_id: &str, song_id: &str) -> Value {
    let response = app
        .post(
            &format!("/setlists/{setlist_id}/songs"),
            token,
            json!({ "song_id": song_id }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
    response.body
}

async fn repertoire_of(app: &TestApp, token: &str, band: &str) -> String {
    app.get(&format!("/bands/{band}"), token).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn song(app: &TestApp, token: &str, id: &str) -> Value {
    let response = app.get(&format!("/songs/{id}"), token).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    response.body
}

#[tokio::test]
async fn removing_a_song_from_the_repertoire_moves_it_to_the_band_trash() {
    let app = app!();
    let (_, owner) = app.user("repowner", Role::User).await;
    let band = app.band(&owner, "Trash Rep").await;
    let repertoire = repertoire_of(&app, &owner, &band).await;
    let setlist = app.setlist(&owner, "Show", Some(&band)).await;

    // A song with only some of its details, added to a band setlist.
    let mine = app.song_id(&owner, "Autor", "Balada").await;
    let added = add(&app, &owner, &setlist, &mine).await;
    assert_eq!(added["band_copy"], "created");
    let first_copy = added["song_id"].as_str().unwrap().to_string();
    assert_ne!(first_copy, mine);

    // The personal song is completed later.
    let updated = app
        .patch(
            &format!("/songs/{mine}"),
            &owner,
            json!({ "tempo": 92, "tonality": "Am", "lyrics": "[Am]Letra" }),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);

    // Taking it out of the repertoire takes it out of the band: it leaves
    // every band setlist and lands in the band's trash.
    let removed = app
        .delete(
            &format!("/setlists/{repertoire}/songs/{first_copy}"),
            &owner,
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    assert!(app.setlist_titles(&owner, &repertoire).await.is_empty());
    assert!(app.setlist_titles(&owner, &setlist).await.is_empty());
    let trash = app
        .get(&format!("/trash?scope=band&band_id={band}"), &owner)
        .await;
    assert_eq!(trash.body["meta"]["total_items"], 1, "{}", trash.body);
    assert_eq!(trash.body["data"][0]["id"], first_copy.as_str());

    // Adding the personal song again brings its current version.
    let again = add(&app, &owner, &setlist, &mine).await;
    assert_eq!(again["band_copy"], "created");
    let second_copy = again["song_id"].as_str().unwrap().to_string();
    assert_ne!(second_copy, first_copy);
    let copy = song(&app, &owner, &second_copy).await;
    assert_eq!(copy["tempo"], 92);
    assert_eq!(copy["tonality"], "Am");
    assert_eq!(copy["lyrics"], "[Am]Letra");
    assert_eq!(
        app.setlist_titles(&owner, &repertoire).await,
        vec!["Balada"]
    );

    // The old copy can't come back next to the new one...
    let conflict = app
        .post(
            &format!("/trash/song/{first_copy}/restore"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    assert_eq!(conflict.code(), "RESTORE_CONFLICT");

    // ...but restoring a removed song puts it back where it was.
    app.delete(
        &format!("/setlists/{repertoire}/songs/{second_copy}"),
        &owner,
    )
    .await;
    let restored = app
        .post(
            &format!("/trash/song/{second_copy}/restore"),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(restored.status, StatusCode::NO_CONTENT, "{}", restored.body);
    assert_eq!(
        app.setlist_titles(&owner, &repertoire).await,
        vec!["Balada"]
    );
    assert_eq!(app.setlist_titles(&owner, &setlist).await, vec!["Balada"]);

    // Other setlists only unlink: the song stays in the repertoire.
    let unlinked = app
        .delete(&format!("/setlists/{setlist}/songs/{second_copy}"), &owner)
        .await;
    assert_eq!(unlinked.status, StatusCode::NO_CONTENT);
    assert!(app.setlist_titles(&owner, &setlist).await.is_empty());
    assert_eq!(
        app.setlist_titles(&owner, &repertoire).await,
        vec!["Balada"]
    );
}

#[tokio::test]
async fn removing_from_the_repertoire_needs_manage_songs() {
    let app = app!();
    let (_, owner) = app.user("permowner", Role::User).await;
    let (_, member) = app.user("permmember", Role::User).await;
    let band = app.band(&owner, "Perm Rep").await;
    app.join_band(&owner, &member, &band, None).await;
    let repertoire = repertoire_of(&app, &owner, &band).await;
    // Setlists, but not songs.
    let allowed = app
        .put(
            &format!("/bands/{band}/permissions"),
            &owner,
            json!({ "permissions": [
                { "role": "member", "permission": "manage_setlists", "allowed": true },
                { "role": "member", "permission": "manage_songs", "allowed": false }
            ] }),
        )
        .await;
    assert!(allowed.status.is_success(), "{}", allowed.body);

    let mine = app.song_id(&owner, "Autor", "Fica").await;
    let copy = add(&app, &owner, &repertoire, &mine).await["song_id"]
        .as_str()
        .unwrap()
        .to_string();

    let denied = app
        .delete(&format!("/setlists/{repertoire}/songs/{copy}"), &member)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);
    assert_eq!(app.setlist_titles(&owner, &repertoire).await, vec!["Fica"]);
}

#[tokio::test]
async fn adding_a_song_again_brings_an_untouched_band_copy_up_to_date() {
    let app = app!();
    let (_, owner) = app.user("syncowner", Role::User).await;
    let band = app.band(&owner, "Sync Band").await;
    let first = app.setlist(&owner, "Primeiro", Some(&band)).await;
    let second = app.setlist(&owner, "Segundo", Some(&band)).await;

    let mine = app.song_id(&owner, "Autor", "Rascunho").await;
    let copy = add(&app, &owner, &first, &mine).await["song_id"]
        .as_str()
        .unwrap()
        .to_string();

    // In step: reused as it is.
    let status = app.get(&format!("/songs/{mine}/band-copies"), &owner).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    assert_eq!(status.body[0]["song_id"], copy.as_str());
    assert_eq!(status.body[0]["band_name"], "Sync Band");
    assert_eq!(status.body[0]["has_updates"], false);
    assert_eq!(status.body[0]["band_edited"], false);
    assert_eq!(status.body[0]["can_update"], true);

    app.patch(
        &format!("/songs/{mine}"),
        &owner,
        json!({ "title": "Versão final", "tempo": 120, "tags": ["show"] }),
    )
    .await;
    let status = app.get(&format!("/songs/{copy}/band-copies"), &owner).await;
    assert_eq!(status.body[0]["has_updates"], true);
    let updates = app
        .get(&format!("/bands/{band}/song-updates"), &owner)
        .await;
    assert_eq!(updates.status, StatusCode::OK, "{}", updates.body);
    assert_eq!(updates.body.as_array().unwrap().len(), 1);

    // The band never edited its copy: it simply follows the original.
    let added = add(&app, &owner, &second, &mine).await;
    assert_eq!(added["song_id"], copy.as_str());
    assert_eq!(added["band_copy"], "updated");
    let updated = song(&app, &owner, &copy).await;
    assert_eq!(updated["title"], "Versão final");
    assert_eq!(updated["tempo"], 120);
    assert_eq!(updated["tags"], json!(["show"]));
    assert_eq!(
        app.setlist_titles(&owner, &first).await,
        vec!["Versão final"]
    );
    let status = app.get(&format!("/songs/{mine}/band-copies"), &owner).await;
    assert_eq!(status.body[0]["has_updates"], false);
    assert!(
        app.get(&format!("/bands/{band}/song-updates"), &owner)
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn band_edits_are_kept_until_the_contributor_updates_the_copy() {
    let app = app!();
    let (_, owner) = app.user("editowner", Role::User).await;
    let (_, member) = app.user("editmember", Role::User).await;
    let band = app.band(&owner, "Edit Band").await;
    app.join_band(&owner, &member, &band, Some("admin")).await;
    let first = app.setlist(&owner, "Primeiro", Some(&band)).await;
    let second = app.setlist(&owner, "Segundo", Some(&band)).await;

    let mine = app.song_id(&owner, "Autor", "Arranjo").await;
    let copy = add(&app, &owner, &first, &mine).await["song_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The band adapts its copy, and the original changes too.
    let edited = app
        .patch(
            &format!("/songs/{copy}"),
            &member,
            json!({ "tonality": "D" }),
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
    app.patch(&format!("/songs/{mine}"), &owner, json!({ "tempo": 100 }))
        .await;

    // Adding it again doesn't overwrite the band's edits.
    let added = add(&app, &owner, &second, &mine).await;
    assert_eq!(added["band_copy"], "outdated");
    let kept = song(&app, &owner, &copy).await;
    assert_eq!(kept["tonality"], "D");
    assert!(kept["tempo"].is_null());
    let status = app.get(&format!("/songs/{copy}/band-copies"), &owner).await;
    assert_eq!(status.body[0]["has_updates"], true);
    assert_eq!(status.body[0]["band_edited"], true);

    // Another member's songs stay private, and only the contributor can
    // pull their original in.
    let theirs = app
        .get(&format!("/songs/{copy}/band-copies"), &member)
        .await;
    assert_eq!(theirs.status, StatusCode::OK);
    assert_eq!(theirs.body, json!([]));
    assert!(
        app.get(&format!("/bands/{band}/song-updates"), &member)
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
    let refused = app
        .post(&format!("/songs/{copy}/sync"), &member, json!({}))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    assert_eq!(refused.code(), "SONG_ORIGINAL_UNAVAILABLE");

    // The contributor takes their version, on request.
    let synced = app
        .post(&format!("/songs/{copy}/sync"), &owner, json!({}))
        .await;
    assert_eq!(synced.status, StatusCode::OK, "{}", synced.body);
    assert_eq!(synced.body["has_updates"], false);
    assert_eq!(synced.body["band_edited"], false);
    let replaced = song(&app, &owner, &copy).await;
    assert!(replaced["tonality"].is_null());
    assert_eq!(replaced["tempo"], 100);
    // Still in every setlist it was in.
    assert_eq!(app.setlist_titles(&owner, &first).await, vec!["Arranjo"]);
    assert_eq!(app.setlist_titles(&owner, &second).await, vec!["Arranjo"]);

    // Without the original, there's nothing to update from.
    app.delete(&format!("/songs/{mine}"), &owner).await;
    let gone = app
        .post(&format!("/songs/{copy}/sync"), &owner, json!({}))
        .await;
    assert_eq!(gone.status, StatusCode::CONFLICT);
    assert_eq!(gone.code(), "SONG_ORIGINAL_UNAVAILABLE");
}

#[tokio::test]
async fn updating_a_copy_follows_the_original_artist_into_the_band() {
    let app = app!();
    let (_, owner) = app.user("artowner", Role::User).await;
    let band = app.band(&owner, "Artist Band").await;
    let setlist = app.setlist(&owner, "Show", Some(&band)).await;

    let mine = app.song_id(&owner, "Antigo", "Cover").await;
    let copy = add(&app, &owner, &setlist, &mine).await["song_id"]
        .as_str()
        .unwrap()
        .to_string();
    let other_artist = app.artist(&owner, "Novo").await;
    app.patch(
        &format!("/songs/{mine}"),
        &owner,
        json!({ "artist_id": other_artist }),
    )
    .await;

    let synced = app
        .post(&format!("/songs/{copy}/sync"), &owner, json!({}))
        .await;
    assert_eq!(synced.status, StatusCode::OK, "{}", synced.body);
    let updated = song(&app, &owner, &copy).await;
    assert_eq!(updated["artist_name"], "Novo");
    // The band's own artist, not the member's personal one.
    assert_ne!(updated["artist_id"], other_artist.as_str());
}
