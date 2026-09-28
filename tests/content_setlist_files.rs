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

#[tokio::test]
async fn a_gig_file_brings_its_setlist_along() {
    let app = app!();
    let (_, owner) = app.user("gigexport", Role::User).await;
    let wave = app.song_id(&owner, "Tom Jobim", "Wave").await;
    let setlist = app.setlist(&owner, "Bossa", None).await;
    app.add_to_setlist(&owner, &setlist, &wave).await;
    let gig = app
        .post(
            "/gigs",
            &owner,
            json!({
                "venue": "Blue Note",
                "location": "Rio de Janeiro",
                "scheduled_at": "2030-05-10T21:00:00",
                "notes": "Passagem de som às 18h",
                "setlist_id": setlist
            }),
        )
        .await;
    assert_eq!(gig.status, StatusCode::CREATED, "{}", gig.body);
    let gig_id = gig.body["id"].as_str().unwrap().to_string();

    let file = app.get(&format!("/gigs/{gig_id}/export"), &owner).await;
    assert_eq!(file.status, StatusCode::OK, "{}", file.body);
    assert_eq!(file.body["kind"], "gig");
    assert_eq!(file.body["setlists"].as_array().unwrap().len(), 1);
    assert_eq!(file.body["gigs"][0]["setlist_id"], setlist.as_str());

    // Not a setlist file.
    let (_, other) = app.user("gigimport", Role::User).await;
    let wrong = app
        .post("/setlists/import", &other, file.body.clone())
        .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST, "{}", wrong.body);

    let imported = app.post("/gigs/import", &other, file.body).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    let new_gig = imported.body["gig_ids"][0].as_str().unwrap();
    let new_gig = app.get(&format!("/gigs/{new_gig}"), &other).await;
    assert_eq!(new_gig.body["venue"], "Blue Note");
    assert_eq!(new_gig.body["location"], "Rio de Janeiro");
    assert_eq!(new_gig.body["notes"], "Passagem de som às 18h");
    let new_setlist = new_gig.body["setlist_id"].as_str().unwrap().to_string();
    assert_eq!(
        new_setlist,
        imported.body["setlist_ids"][0].as_str().unwrap()
    );
    assert_eq!(app.setlist_titles(&other, &new_setlist).await, vec!["Wave"]);
}

#[tokio::test]
async fn a_tour_file_rebuilds_the_tour_with_its_gigs_and_setlists() {
    let app = app!();
    let (_, owner) = app.user("tourexport", Role::User).await;
    let tour = app
        .post(
            "/tours",
            &owner,
            json!({ "name": "Turnê de Verão", "description": "Litoral", "start_date": "2030-01-01", "end_date": "2030-02-28" }),
        )
        .await;
    assert_eq!(tour.status, StatusCode::CREATED, "{}", tour.body);
    let tour_id = tour.body["id"].as_str().unwrap().to_string();

    let song = app.song_id(&owner, "Artista", "Abertura").await;
    let encore = app.song_id(&owner, "Artista", "Bis").await;
    let main = app.setlist(&owner, "Principal", None).await;
    app.add_to_setlist(&owner, &main, &song).await;
    app.add_to_setlist(&owner, &main, &encore).await;
    let short = app.setlist(&owner, "Curto", None).await;
    app.add_to_setlist(&owner, &short, &song).await;
    for (venue, date, setlist) in [
        ("Praia", "2030-01-10T21:00:00", Some(&main)),
        ("Serra", "2030-01-20T21:00:00", Some(&main)),
        ("Festival", "2030-02-01T18:00:00", Some(&short)),
        ("Bar", "2030-02-10T22:00:00", None),
    ] {
        let gig = app
            .post(
                "/gigs",
                &owner,
                json!({ "venue": venue, "scheduled_at": date, "tour_id": tour_id, "setlist_id": setlist }),
            )
            .await;
        assert_eq!(gig.status, StatusCode::CREATED, "{}", gig.body);
    }

    let file = app.get(&format!("/tours/{tour_id}/export"), &owner).await;
    assert_eq!(file.status, StatusCode::OK, "{}", file.body);
    assert_eq!(file.body["kind"], "tour");
    assert_eq!(file.body["gigs"].as_array().unwrap().len(), 4);
    // Each setlist and song once, however many gigs play it.
    assert_eq!(file.body["setlists"].as_array().unwrap().len(), 2);
    assert_eq!(file.body["songs"].as_array().unwrap().len(), 2);

    let (_, other) = app.user("tourimport", Role::User).await;
    let imported = app.post("/tours/import", &other, file.body).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    assert_eq!(imported.body["gig_ids"].as_array().unwrap().len(), 4);
    let new_tour = imported.body["tour_ids"][0].as_str().unwrap();
    let detail = app.get(&format!("/tours/{new_tour}"), &other).await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(detail.body["name"], "Turnê de Verão");
    assert_eq!(detail.body["description"], "Litoral");
    let gigs = detail.body["gigs"].as_array().unwrap();
    let gig = |venue: &str| gigs.iter().find(|g| g["venue"] == venue).unwrap().clone();
    assert_eq!(gig("Praia")["setlist"]["title"], "Principal");
    assert_eq!(gig("Festival")["setlist"]["title"], "Curto");
    assert!(gig("Bar")["setlist"].is_null());
    // The two gigs share one setlist, as in the original.
    assert_eq!(gig("Praia")["setlist"]["id"], gig("Serra")["setlist"]["id"]);
    let setlists = app.get("/setlists", &other).await;
    assert_eq!(setlists.body["meta"]["total_items"], 2);
}

#[tokio::test]
async fn shared_files_must_hold_what_they_say() {
    let app = app!();
    let (_, user) = app.user("forjador", Role::User).await;
    let song = app.song_id(&user, "Artista", "Canção").await;
    let setlist = app.setlist(&user, "Show", None).await;
    app.add_to_setlist(&user, &setlist, &song).await;
    let file = app.get(&format!("/setlists/{setlist}/export"), &user).await;

    // A gig file whose gig doesn't play the setlist it carries.
    let mut forged = file.body.clone();
    forged["kind"] = json!("gig");
    forged["gigs"] = json!([{
        "id": "00000000-0000-0000-0000-000000000001",
        "venue": "Bar",
        "scheduled_at": "2030-01-01T20:00:00",
        "status": "confirmed",
        "setlist_id": null
    }]);
    let refused = app.post("/gigs/import", &user, forged).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
}
