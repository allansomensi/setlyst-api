//! A song's manual harmonic analysis (`/songs/{id}/analysis`).

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

fn doc(degree: &str) -> Value {
    json!({
        "version": 1,
        "key": "C",
        "chords": [
            { "chord": "G7", "degree": degree, "arrow": "resolution", "target": 1 },
            { "chord": "C", "degree": "I" }
        ],
        "notes": "Dominante primário resolvendo na tônica."
    })
}

fn path(song: &str) -> String {
    format!("/songs/{song}/analysis")
}

#[tokio::test]
async fn analysis_roundtrip_and_delete() {
    let app = app!();
    let (_, user) = app.user("analyst", Role::User).await;
    let song = app.song_id(&user, "Tom Jobim", "Wave").await;

    let empty = app.get(&path(&song), &user).await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
    assert!(empty.body.is_null(), "{}", empty.body);

    let saved = app
        .put(&path(&song), &user, json!({ "content": doc("V7") }))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["song_id"], song.as_str());
    assert_eq!(saved.body["content"], doc("V7"));
    assert_eq!(saved.body["updated_by_username"], "analyst");
    assert_eq!(saved.body["created_at"], saved.body["updated_at"]);

    let read = app.get(&path(&song), &user).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body, saved.body);

    // Last write wins without a base version.
    let replaced = app
        .put(&path(&song), &user, json!({ "content": doc("SubV7") }))
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.body);
    assert_eq!(replaced.body["content"], doc("SubV7"));
    assert_eq!(replaced.body["created_at"], saved.body["created_at"]);

    let deleted = app.delete(&path(&song), &user).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert!(app.get(&path(&song), &user).await.body.is_null());
    // Idempotent.
    let again = app.delete(&path(&song), &user).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT, "{}", again.body);
}

#[tokio::test]
async fn analysis_content_is_a_bounded_object() {
    let app = app!();
    let (_, user) = app.user("bounded", Role::User).await;
    let song = app.song_id(&user, "Autor", "Música").await;

    for content in [json!([1, 2]), json!("texto"), json!(null), json!(3)] {
        let refused = app
            .put(&path(&song), &user, json!({ "content": content }))
            .await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
        assert_eq!(refused.code(), "VALIDATION_ERROR");
        assert!(refused.body["meta"]["fields"]["content"].is_array());
    }

    // Over 256 KiB serialized, but under the 512 KiB body limit.
    let big = json!({ "notes": "x".repeat(300 * 1024) });
    let too_big = app
        .put(&path(&song), &user, json!({ "content": big }))
        .await;
    assert_eq!(too_big.status, StatusCode::BAD_REQUEST, "{}", too_big.body);
    assert_eq!(too_big.code(), "VALIDATION_ERROR");

    // Over the body limit: refused before parsing.
    let huge = json!({ "notes": "x".repeat(600 * 1024) });
    let too_huge = app
        .put(&path(&song), &user, json!({ "content": huge }))
        .await;
    assert_eq!(too_huge.status, StatusCode::PAYLOAD_TOO_LARGE);

    let deep = json!({ "a": [[[[[[[[1]]]]]]]] });
    let too_deep = app
        .put(&path(&song), &user, json!({ "content": deep }))
        .await;
    assert_eq!(
        too_deep.status,
        StatusCode::BAD_REQUEST,
        "{}",
        too_deep.body
    );

    assert!(app.get(&path(&song), &user).await.body.is_null());
}

#[tokio::test]
async fn stale_base_version_is_a_conflict() {
    let app = app!();
    let (_, user) = app.user("concurrent", Role::User).await;
    let song = app.song_id(&user, "Autor", "Conflito").await;

    // A base version with nothing stored yet just creates it.
    let first = app
        .put(
            &path(&song),
            &user,
            json!({ "content": doc("V7"), "base_updated_at": "2020-01-01T00:00:00" }),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let v1 = first.body["updated_at"].clone();

    // Tab A saves on top of v1.
    let second = app
        .put(
            &path(&song),
            &user,
            json!({ "content": doc("II-V"), "base_updated_at": v1 }),
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    let v2 = second.body["updated_at"].clone();
    assert_ne!(v1, v2);

    // Tab B still edits v1.
    let stale = app
        .put(
            &path(&song),
            &user,
            json!({ "content": doc("stale"), "base_updated_at": v1 }),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert_eq!(stale.code(), "ANALYSIS_CONFLICT");
    assert_eq!(stale.body["meta"]["updated_at"], v2);
    assert_eq!(
        app.get(&path(&song), &user).await.body["content"],
        doc("II-V")
    );

    // With the current version it goes through; the value read back
    // matches the one returned by the save.
    let current = app.get(&path(&song), &user).await.body["updated_at"].clone();
    assert_eq!(current, v2);
    let fresh = app
        .put(
            &path(&song),
            &user,
            json!({ "content": doc("fresh"), "base_updated_at": current }),
        )
        .await;
    assert_eq!(fresh.status, StatusCode::OK, "{}", fresh.body);
}

#[tokio::test]
async fn analysis_follows_the_song_access() {
    let app = app!();
    let (_, owner) = app.user("aowner", Role::User).await;
    let (_, stranger) = app.user("astranger", Role::User).await;
    let song = app.song_id(&owner, "Autor", "Privada").await;
    let saved = app
        .put(&path(&song), &owner, json!({ "content": doc("V7") }))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    // Someone else's personal song doesn't exist for them.
    let read = app.get(&path(&song), &stranger).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND, "{}", read.body);
    let write = app
        .put(&path(&song), &stranger, json!({ "content": doc("x") }))
        .await;
    assert_eq!(write.status, StatusCode::NOT_FOUND, "{}", write.body);
    let delete = app.delete(&path(&song), &stranger).await;
    assert_eq!(delete.status, StatusCode::NOT_FOUND, "{}", delete.body);

    // Trashed: gone for the owner too; permanently deleted: the analysis
    // goes with it.
    let trashed = app.delete(&format!("/songs/{song}"), &owner).await;
    assert_eq!(trashed.status, StatusCode::NO_CONTENT, "{}", trashed.body);
    let read = app.get(&path(&song), &owner).await;
    assert_eq!(read.status, StatusCode::NOT_FOUND, "{}", read.body);
    let write = app
        .put(&path(&song), &owner, json!({ "content": doc("x") }))
        .await;
    assert_eq!(write.status, StatusCode::NOT_FOUND, "{}", write.body);

    let count = |app: &TestApp, song: String| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM song_analyses WHERE song_id = $1::uuid",
            )
            .bind(song)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(count(&app, song.clone()).await, 1);
    let purged = app.delete(&format!("/trash/song/{song}"), &owner).await;
    assert_eq!(purged.status, StatusCode::NO_CONTENT, "{}", purged.body);
    assert_eq!(count(&app, song.clone()).await, 0);
}

#[tokio::test]
async fn band_songs_members_read_managers_write() {
    let app = app!();
    let (_, owner) = app.user("bowner", Role::User).await;
    let (_, member) = app.user("bmember", Role::User).await;
    let (_, moderator) = app.user("bmoderator", Role::User).await;
    let (_, outsider) = app.user("boutsider", Role::User).await;
    let band = app.band(&owner, "Harmonia").await;
    app.join_band(&owner, &member, &band, None).await;
    app.join_band(&owner, &moderator, &band, Some("moderator"))
        .await;

    // The owner's personal song, analysed, then added to a band setlist:
    // the band's copy starts without an analysis.
    let mine = app.song_id(&owner, "Autor", "Da Banda").await;
    let personal = app
        .put(&path(&mine), &owner, json!({ "content": doc("V7") }))
        .await;
    assert_eq!(personal.status, StatusCode::OK, "{}", personal.body);
    let setlist = app.setlist(&owner, "Show", Some(&band)).await;
    let added = app
        .post(
            &format!("/setlists/{setlist}/songs"),
            &owner,
            json!({ "song_id": mine }),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(added.body["band_copy"], "created");
    let copy = added.body["song_id"].as_str().unwrap().to_string();
    let empty = app.get(&path(&copy), &member).await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
    assert!(empty.body.is_null(), "{}", empty.body);

    // A moderator manages songs by default.
    let saved = app
        .put(&path(&copy), &moderator, json!({ "content": doc("IV") }))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.body["updated_by_username"], "bmoderator");

    // A plain member reads it, but can't change it.
    let read = app.get(&path(&copy), &member).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["content"], doc("IV"));
    let denied = app
        .put(&path(&copy), &member, json!({ "content": doc("x") }))
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);
    let denied = app.delete(&path(&copy), &member).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);

    // Outside the band it doesn't exist.
    let hidden = app.get(&path(&copy), &outsider).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    let hidden = app
        .put(&path(&copy), &outsider, json!({ "content": doc("x") }))
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);

    // The owner's personal analysis was left alone.
    assert_eq!(
        app.get(&path(&mine), &owner).await.body["content"],
        doc("V7")
    );
}

#[tokio::test]
async fn versions_start_without_an_analysis() {
    let app = app!();
    let (_, user) = app.user("aversions", Role::User).await;
    let artist = app.artist(&user, "Autor").await;
    let original = app.song_id(&user, "Autor", "Original").await;
    let saved = app
        .put(&path(&original), &user, json!({ "content": doc("V7") }))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    let version = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Original",
                "artist_id": artist,
                "version_of": original,
                "version_label": "Acústica"
            }),
        )
        .await;
    assert_eq!(version.status, StatusCode::CREATED, "{}", version.body);
    let id = version.body["id"].as_str().unwrap();
    assert!(app.get(&path(id), &user).await.body.is_null());
}

#[tokio::test]
async fn backups_carry_the_analysis() {
    let app = app!();
    let (_, source) = app.user("bsource", Role::User).await;
    let (_, target) = app.user("btarget", Role::User).await;
    let song = app.song_id(&source, "Autor", "Com Análise").await;
    app.song_id(&source, "Autor", "Sem Análise").await;
    let saved = app
        .put(&path(&song), &source, json!({ "content": doc("V7") }))
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    let export = app.get("/backup/export", &source).await;
    assert_eq!(export.status, StatusCode::OK, "{}", export.body);
    let songs = export.body["songs"].as_array().unwrap();
    let with = songs.iter().find(|s| s["title"] == "Com Análise").unwrap();
    let without = songs.iter().find(|s| s["title"] == "Sem Análise").unwrap();
    assert_eq!(with["analysis"], doc("V7"));
    assert!(without["analysis"].is_null());

    let imported = app.post("/backup/import", &target, export.body).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    let listed = app.get("/songs?per_page=100", &target).await;
    let id = listed.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["title"] == "Com Análise")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = app.get(&path(&id), &target).await;
    assert_eq!(read.body["content"], doc("V7"));
    assert_eq!(read.body["updated_by_username"], "btarget");

    // A backup with an invalid analysis is refused as a whole.
    let mut bad = app.get("/backup/export", &source).await.body;
    bad["songs"][0]["analysis"] = json!([1]);
    let refused = app.post("/backup/import", &target, bad).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
}
