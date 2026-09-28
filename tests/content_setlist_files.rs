//! A setlist exported as a file and imported into another account.

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

/// The running order as `(item_type, title or label)`.
async fn running_order(app: &TestApp, token: &str, setlist: &str) -> Vec<(String, String)> {
    let items = app.get(&format!("/setlists/{setlist}/items"), token).await;
    assert_eq!(items.status, StatusCode::OK, "{}", items.body);
    items
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            let kind = item["item_type"].as_str().unwrap().to_string();
            let text = match kind.as_str() {
                "song" => item["song"]["title"].clone(),
                "block" => item["name"].clone(),
                _ => item["label"].clone(),
            };
            (kind, text.as_str().unwrap_or_default().to_string())
        })
        .collect()
}

#[tokio::test]
async fn a_setlist_file_rebuilds_the_setlist_in_an_empty_account() {
    let app = app!();
    let (_, owner) = app.user("exporter", Role::User).await;

    let wave = app.song_id(&owner, "Tom Jobim", "Wave").await;
    app.patch(
        &format!("/songs/{wave}"),
        &owner,
        json!({ "lyrics": "[D7M]Vou te contar", "tonality": "D", "tempo": 120 }),
    )
    .await;
    let analysis = json!({ "schema": 1, "entries": { "0": { "fn": "T" } } });
    let saved = app
        .put(
            &format!("/songs/{wave}/analysis"),
            &owner,
            json!({ "content": analysis }),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    let garota = app.song_id(&owner, "Tom Jobim", "Garota de Ipanema").await;
    let trem = app.song_id(&owner, "Milton Nascimento", "Trem Azul").await;

    let setlist = app.setlist(&owner, "Bossa", None).await;
    let block = app
        .post(
            &format!("/setlists/{setlist}/blocks"),
            &owner,
            json!({ "name": "Abertura" }),
        )
        .await;
    assert_eq!(block.status, StatusCode::CREATED, "{}", block.body);
    app.add_to_setlist(&owner, &setlist, &wave).await;
    app.add_to_setlist(&owner, &setlist, &garota).await;
    let pause = app
        .post(
            &format!("/setlists/{setlist}/breaks"),
            &owner,
            json!({ "label": "Água", "duration_minutes": 10 }),
        )
        .await;
    assert_eq!(pause.status, StatusCode::CREATED, "{}", pause.body);
    app.add_to_setlist(&owner, &setlist, &trem).await;
    app.patch(
        &format!("/setlists/{setlist}/songs/{garota}"),
        &owner,
        json!({ "transpose": -2 }),
    )
    .await;

    let file = app
        .get(&format!("/setlists/{setlist}/export"), &owner)
        .await;
    assert_eq!(file.status, StatusCode::OK, "{}", file.body);
    assert_eq!(file.body["kind"], "setlist");
    assert_eq!(file.body["version"], 5);
    assert_eq!(file.body["artists"].as_array().unwrap().len(), 2);
    assert_eq!(file.body["songs"].as_array().unwrap().len(), 3);
    assert!(file.body["gigs"].as_array().unwrap().is_empty());

    let (_, other) = app.user("importer", Role::User).await;
    let imported = app
        .post("/setlists/import", &other, file.body.clone())
        .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    let new_setlist = imported.body["setlist_ids"][0]
        .as_str()
        .unwrap()
        .to_string();

    // Same running order, blocks and breaks included.
    assert_eq!(
        running_order(&app, &other, &new_setlist).await,
        running_order(&app, &owner, &setlist).await
    );
    let items = app
        .get(&format!("/setlists/{new_setlist}/items"), &other)
        .await;
    let items = items.body.as_array().unwrap().clone();
    let pause = items.iter().find(|i| i["item_type"] == "break").unwrap();
    assert_eq!(pause["duration_minutes"], 10);
    let song = |title: &str| -> Value {
        items
            .iter()
            .find(|i| i["item_type"] == "song" && i["song"]["title"] == title)
            .map(|i| i["song"].clone())
            .unwrap()
    };
    assert_eq!(song("Garota de Ipanema")["transpose"], -2);
    let wave_copy = song("Wave");
    assert_eq!(wave_copy["lyrics"], "[D7M]Vou te contar");
    assert_eq!(wave_copy["tempo"], 120);
    assert_eq!(wave_copy["artist_name"], "Tom Jobim");

    // The songs and artists are the importer's own now.
    let copied_analysis = app
        .get(
            &format!("/songs/{}/analysis", wave_copy["id"].as_str().unwrap()),
            &other,
        )
        .await;
    assert_eq!(copied_analysis.status, StatusCode::OK);
    assert_eq!(copied_analysis.body["content"], analysis);
    let artists = app.get("/artists?per_page=100", &other).await;
    assert_eq!(artists.body["data"].as_array().unwrap().len(), 2);

    // Imported again: the songs and artists are reused, the title isn't.
    let again = app.post("/setlists/import", &other, file.body).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    let songs = app.get("/songs?per_page=100", &other).await;
    assert_eq!(songs.body["data"].as_array().unwrap().len(), 3);
    let second = again.body["setlist_ids"][0].as_str().unwrap();
    let second = app.get(&format!("/setlists/{second}"), &other).await;
    assert_eq!(second.body["title"], "Bossa (2)");
}

#[tokio::test]
async fn setlist_files_are_for_those_who_can_see_the_setlist() {
    let app = app!();
    let (_, owner) = app.user("dono", Role::User).await;
    let (_, stranger) = app.user("estranho", Role::User).await;
    let setlist = app.setlist(&owner, "Privada", None).await;

    let refused = app
        .get(&format!("/setlists/{setlist}/export"), &stranger)
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);
}

#[tokio::test]
async fn a_full_backup_is_not_a_setlist_file() {
    let app = app!();
    let (_, user) = app.user("misturado", Role::User).await;
    let song = app.song_id(&user, "Artista", "Canção").await;
    let first = app.setlist(&user, "Um", None).await;
    app.add_to_setlist(&user, &first, &song).await;
    app.setlist(&user, "Dois", None).await;

    let backup = app.get("/backup/export", &user).await;
    assert_eq!(backup.status, StatusCode::OK, "{}", backup.body);
    let refused = app.post("/setlists/import", &user, backup.body).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
}

#[tokio::test]
async fn full_backups_keep_blocks_and_breaks() {
    let app = app!();
    let (_, user) = app.user("blocos", Role::User).await;
    let song = app.song_id(&user, "Artista", "Canção").await;
    let setlist = app.setlist(&user, "Show", None).await;
    app.post(
        &format!("/setlists/{setlist}/blocks"),
        &user,
        json!({ "name": "Bis" }),
    )
    .await;
    app.add_to_setlist(&user, &setlist, &song).await;

    let backup = app.get("/backup/export", &user).await;
    assert_eq!(backup.body["kind"], "backup");
    let (_, other) = app.user("restaurado", Role::User).await;
    let imported = app.post("/backup/import", &other, backup.body).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    let restored = imported.body["setlist_ids"][0].as_str().unwrap();
    assert_eq!(
        running_order(&app, &other, restored).await,
        vec![
            ("block".to_string(), "Bis".to_string()),
            ("song".to_string(), "Canção".to_string()),
        ]
    );
}
