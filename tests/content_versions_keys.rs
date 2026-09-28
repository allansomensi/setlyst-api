//! Song versions and the key a setlist plays each song in.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;
use setlyst_api::models::user::Role;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn versions_of_a_song_form_one_family() {
    let app = app!();
    let (_, user) = app.user("versions", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let original = app.song_id(&user, "Artista", "Canção").await;

    let simple = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Canção",
                "artist_id": artist,
                "version_of": original,
                "version_label": "  Simplificada ",
                "lyrics": "[G]Lá"
            }),
        )
        .await;
    assert_eq!(simple.status, StatusCode::CREATED, "{}", simple.body);
    assert_eq!(simple.body["version_label"], "Simplificada");
    assert_eq!(simple.body["version_of"], original.as_str());
    let simple_id = simple.body["id"].as_str().unwrap().to_string();

    // Same title, artist and label: a duplicate, whatever the case.
    let duplicate = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "canção",
                "artist_id": artist,
                "version_of": original,
                "version_label": "simplificada"
            }),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.body);

    // A version of a version joins the original.
    let acoustic = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Canção",
                "artist_id": artist,
                "version_of": simple_id,
                "version_label": "Acústica"
            }),
        )
        .await;
    assert_eq!(acoustic.status, StatusCode::CREATED, "{}", acoustic.body);
    assert_eq!(acoustic.body["version_of"], original.as_str());

    for id in [&original, &simple_id] {
        let family = app.get(&format!("/songs/{id}/versions"), &user).await;
        assert_eq!(family.status, StatusCode::OK, "{}", family.body);
        let labels: Vec<_> = family
            .body
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v["version_label"].clone(), v["is_original"].clone()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (json!(null), json!(true)),
                (json!("Acústica"), json!(false)),
                (json!("Simplificada"), json!(false)),
            ]
        );
    }

    // Renaming a version onto another's label is a duplicate too; clearing
    // it would collide with the original.
    let clash = app
        .patch(
            &format!("/songs/{simple_id}"),
            &user,
            json!({ "version_label": "ACÚSTICA" }),
        )
        .await;
    assert_eq!(clash.status, StatusCode::CONFLICT, "{}", clash.body);
    let cleared = app
        .patch(
            &format!("/songs/{simple_id}"),
            &user,
            json!({ "version_label": null }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::CONFLICT, "{}", cleared.body);
    let renamed = app
        .patch(
            &format!("/songs/{simple_id}"),
            &user,
            json!({ "version_label": "Fácil" }),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);

    let blank = app
        .patch(
            &format!("/songs/{simple_id}"),
            &user,
            json!({ "version_label": "   " }),
        )
        .await;
    assert_eq!(blank.code(), "VALIDATION_ERROR");

    // Someone else's song can't be the original of a version.
    let (_, other) = app.user("stranger", Role::User).await;
    let other_artist = app.artist(&other, "Outro").await;
    let foreign = app
        .post(
            "/songs",
            &other,
            json!({
                "title": "Canção",
                "artist_id": other_artist,
                "version_of": original,
                "version_label": "Minha"
            }),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.body);
    let hidden = app
        .get(&format!("/songs/{original}/versions"), &other)
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_setlist_keeps_the_key_each_song_is_played_in() {
    let app = app!();
    let (_, user) = app.user("keys", Role::User).await;
    let song = app.song_id(&user, "Artista", "Tom").await;
    let other = app.song_id(&user, "Artista", "Outra").await;
    let setlist = app.setlist(&user, "Show", None).await;
    let second = app.setlist(&user, "Ensaio", None).await;
    app.add_to_setlist(&user, &setlist, &song).await;
    app.add_to_setlist(&user, &second, &song).await;

    let saved = app
        .patch(
            &format!("/setlists/{setlist}/songs/{song}"),
            &user,
            json!({ "transpose": -3 }),
        )
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.body);

    let songs = app.get(&format!("/setlists/{setlist}/songs"), &user).await;
    assert_eq!(songs.body["data"][0]["transpose"], -3);
    let items = app.get(&format!("/setlists/{setlist}/items"), &user).await;
    assert_eq!(items.body[0]["song"]["transpose"], -3);
    // Only in that setlist: the song and other setlists are untouched.
    let elsewhere = app.get(&format!("/setlists/{second}/songs"), &user).await;
    assert_eq!(elsewhere.body["data"][0]["transpose"], 0);
    let plain = app.get(&format!("/songs/{song}"), &user).await;
    assert!(plain.body.get("transpose").is_none(), "{}", plain.body);

    for bad in [12, -12] {
        let refused = app
            .patch(
                &format!("/setlists/{setlist}/songs/{song}"),
                &user,
                json!({ "transpose": bad }),
            )
            .await;
        assert_eq!(refused.code(), "VALIDATION_ERROR");
    }
    let missing = app
        .patch(
            &format!("/setlists/{setlist}/songs/{other}"),
            &user,
            json!({ "transpose": 2 }),
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let (_, stranger) = app.user("stranger", Role::User).await;
    let forbidden = app
        .patch(
            &format!("/setlists/{setlist}/songs/{song}"),
            &stranger,
            json!({ "transpose": 2 }),
        )
        .await;
    assert!(
        forbidden.status == StatusCode::NOT_FOUND || forbidden.status == StatusCode::FORBIDDEN,
        "{}",
        forbidden.status
    );

    // A copy of the setlist plays it in the same key.
    let copy = app
        .post(&format!("/setlists/{setlist}/duplicate"), &user, json!({}))
        .await;
    assert_eq!(copy.status, StatusCode::CREATED, "{}", copy.body);
    let copy_id = copy.body["id"].as_str().unwrap();
    let copied = app.get(&format!("/setlists/{copy_id}/songs"), &user).await;
    assert_eq!(copied.body["data"][0]["transpose"], -3);
}

#[tokio::test]
async fn band_setlists_start_in_the_repertoire_key() {
    let app = app!();
    let (_, owner) = app.user("bandkeys", Role::User).await;
    let band = app.band(&owner, "Banda").await;
    let song = app.song_id(&owner, "Artista", "Nossa").await;

    let first = app.setlist(&owner, "Primeiro", Some(&band)).await;
    let added = app
        .post(
            &format!("/setlists/{first}/songs"),
            &owner,
            json!({ "song_id": song }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    let band_song = added.body["song_id"].as_str().unwrap().to_string();

    let repertoire: Uuid =
        sqlx::query_scalar("SELECT id FROM setlists WHERE band_id = $1 AND is_repertoire")
            .bind(Uuid::parse_str(&band).unwrap())
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let saved = app
        .patch(
            &format!("/setlists/{repertoire}/songs/{band_song}"),
            &owner,
            json!({ "transpose": 2 }),
        )
        .await;
    assert_eq!(saved.status, StatusCode::NO_CONTENT, "{}", saved.body);

    let second = app.setlist(&owner, "Segundo", Some(&band)).await;
    app.add_to_setlist(&owner, &second, &band_song).await;
    let songs = app.get(&format!("/setlists/{second}/songs"), &owner).await;
    assert_eq!(songs.body["data"][0]["transpose"], 2, "{}", songs.body);
    // Setlists that already had it keep their own key.
    let untouched = app.get(&format!("/setlists/{first}/songs"), &owner).await;
    assert_eq!(untouched.body["data"][0]["transpose"], 0);
}

#[tokio::test]
async fn backups_keep_versions_and_keys() {
    let app = app!();
    let (_, user) = app.user("backupper", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let original = app.song_id(&user, "Artista", "Canção").await;
    let version = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Canção",
                "artist_id": artist,
                "version_of": original,
                "version_label": "Simplificada"
            }),
        )
        .await;
    assert_eq!(version.status, StatusCode::CREATED, "{}", version.body);
    let version_id = version.body["id"].as_str().unwrap().to_string();
    let setlist = app.setlist(&user, "Show", None).await;
    app.add_to_setlist(&user, &setlist, &version_id).await;
    app.patch(
        &format!("/setlists/{setlist}/songs/{version_id}"),
        &user,
        json!({ "transpose": 5 }),
    )
    .await;

    let backup = app.get("/backup/export", &user).await;
    assert_eq!(backup.status, StatusCode::OK, "{}", backup.body);
    assert_eq!(backup.body["version"], 3);

    let (_, other) = app.user("restorer", Role::User).await;
    let imported = app.post("/backup/import", &other, backup.body).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);

    let songs = app.get("/songs?per_page=100", &other).await;
    let songs = songs.body["data"].as_array().unwrap().clone();
    assert_eq!(songs.len(), 2);
    let restored_original = songs.iter().find(|s| s["version_label"].is_null()).unwrap();
    let restored_version = songs
        .iter()
        .find(|s| s["version_label"] == "Simplificada")
        .unwrap();
    assert_eq!(restored_version["version_of"], restored_original["id"]);

    let setlists = app.get("/setlists", &other).await;
    let restored_setlist = setlists.body["data"][0]["id"].as_str().unwrap();
    let entries = app
        .get(&format!("/setlists/{restored_setlist}/songs"), &other)
        .await;
    assert_eq!(entries.body["data"][0]["transpose"], 5);
}

#[tokio::test]
async fn the_song_list_counts_each_version_family() {
    let app = app!();
    let (_, user) = app.user("contagem", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let original = app.song_id(&user, "Artista", "Canção").await;
    app.song_id(&user, "Artista", "Sozinha").await;
    for label in ["Acústica", "Simplificada"] {
        let version = app
            .post(
                "/songs",
                &user,
                json!({
                    "title": "Canção",
                    "artist_id": artist,
                    "version_of": original,
                    "version_label": label
                }),
            )
            .await;
        assert_eq!(version.status, StatusCode::CREATED, "{}", version.body);
    }

    let list = app.get("/songs", &user).await;
    let songs = list.body["data"].as_array().unwrap();
    let count_of = |title: &str, label: Option<&str>| {
        songs
            .iter()
            .find(|s| s["title"] == title && s["version_label"].as_str() == label)
            .map(|s| s["version_count"].clone())
            .unwrap()
    };
    // Every member of the family reports its size; a lone song, 1.
    assert_eq!(count_of("Canção", None), 3);
    assert_eq!(count_of("Canção", Some("Acústica")), 3);
    assert_eq!(count_of("Sozinha", None), 1);

    // A version in the trash doesn't count.
    let acoustic = songs
        .iter()
        .find(|s| s["version_label"] == "Acústica")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.delete(&format!("/songs/{acoustic}"), &user).await;
    let list = app.get("/songs", &user).await;
    let original_row = list.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == "Canção" && s["version_label"].is_null())
        .unwrap()
        .clone();
    assert_eq!(original_row["version_count"], 2);
}
