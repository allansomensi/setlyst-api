//! Songs: new fields and links, song export, ChordPro import, pins.

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

#[tokio::test]
async fn songs_carry_performance_fields_and_validated_links() {
    let app = app!();
    let (_, user) = app.user("performer", Role::User).await;
    let artist = app.artist(&user, "Artista").await;

    let created = app
        .post(
            "/songs",
            &user,
            json!({
                "title": "Completa",
                "artist_id": artist,
                "energy": 4,
                "time_signature": "6/8",
                "capo": 3,
                "tuning": "  Drop D  ",
                "performance_notes": "Entrada suave",
                "links": [
                    { "url": "https://www.youtube.com/watch?v=abc", "label": "Ao vivo" },
                    { "url": "https://www.youtube.com/watch?v=abc" },
                    { "url": "https://open.spotify.com/track/1" }
                ]
            }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_string();
    let song = app.get(&format!("/songs/{id}"), &user).await;
    assert_eq!(song.body["energy"], 4);
    assert_eq!(song.body["time_signature"], "6/8");
    assert_eq!(song.body["capo"], 3);
    assert_eq!(song.body["tuning"], "Drop D");
    assert_eq!(song.body["performance_notes"], "Entrada suave");
    let links = song.body["links"].as_array().unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(links[0]["provider"], "youtube");
    assert_eq!(links[0]["label"], "Ao vivo");
    assert_eq!(links[1]["provider"], "spotify");
    assert_eq!(song.body["is_pinned"], false);

    for (field, value) in [
        ("energy", json!(6)),
        ("time_signature", json!("11/8")),
        ("capo", json!(12)),
        ("tuning", json!("x".repeat(41))),
        ("performance_notes", json!("x".repeat(2001))),
    ] {
        let bad = app
            .patch(&format!("/songs/{id}"), &user, json!({ field: value }))
            .await;
        assert_eq!(bad.code(), "VALIDATION_ERROR", "{field}");
    }
    let evil = app
        .patch(
            &format!("/songs/{id}"),
            &user,
            json!({ "links": [{ "url": "https://youtube.com.evil.com/x" }] }),
        )
        .await;
    assert_eq!(evil.status, StatusCode::BAD_REQUEST);
    assert_eq!(evil.code(), "INVALID_LINK");
    assert_eq!(evil.body["meta"]["url"], "https://youtube.com.evil.com/x");
    let too_many: Vec<_> = (0..6)
        .map(|i| json!({ "url": format!("https://youtu.be/{i}") }))
        .collect();
    let many = app
        .patch(&format!("/songs/{id}"), &user, json!({ "links": too_many }))
        .await;
    assert_eq!(many.code(), "VALIDATION_ERROR");

    // Clearing.
    let cleared = app
        .patch(
            &format!("/songs/{id}"),
            &user,
            json!({ "energy": null, "time_signature": null, "capo": null, "tuning": null, "performance_notes": null, "links": [] }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    let song = app.get(&format!("/songs/{id}"), &user).await;
    for field in [
        "energy",
        "time_signature",
        "capo",
        "tuning",
        "performance_notes",
    ] {
        assert!(song.body[field].is_null(), "{field}");
    }
    assert!(song.body["links"].as_array().unwrap().is_empty());

    // Setlists take links too.
    let setlist = app
        .post(
            "/setlists",
            &user,
            json!({ "title": "Com links", "links": [{ "url": "https://drive.google.com/file/d/1" }] }),
        )
        .await;
    assert_eq!(setlist.status, StatusCode::CREATED, "{}", setlist.body);
    assert_eq!(setlist.body["links"][0]["provider"], "google_drive");
}

#[tokio::test]
async fn songs_export_to_chordpro_and_pdf() {
    let app = app!();
    let (user_id, user) = app.user("exporter", Role::User).await;
    let artist = app.artist(&user, "Jobim").await;
    let song = app
        .post(
            "/songs",
            &user,
            json!({ "title": "Águas de Março", "artist_id": artist, "tonality": "C", "tempo": 120,
                    "time_signature": "2/4", "capo": 2, "duration": 185, "energy": 3,
                    "lyrics": "[C]É pau, é [G7]pedra" }),
        )
        .await;
    let id = song.body["id"].as_str().unwrap().to_string();

    let cho = app
        .get(&format!("/songs/{id}/export/chordpro"), &user)
        .await;
    assert_eq!(cho.status, StatusCode::OK);
    let text = String::from_utf8(cho.bytes.clone()).unwrap();
    for directive in [
        "{title: Águas de Março}",
        "{artist: Jobim}",
        "{key: C}",
        "{tempo: 120}",
        "{time: 2/4}",
        "{capo: 2}",
        "{duration: 3:05}",
        "{meta: energy 3}",
    ] {
        assert!(text.contains(directive), "{directive}\n{text}");
    }
    let disposition = cho.headers["content-disposition"].to_str().unwrap();
    assert!(disposition.contains("filename*=UTF-8''song-%C3%A1guas-de-mar%C3%A7o.cho"));
    let bulk = app.get("/songs/export/chordpro", &user).await;
    assert!(
        String::from_utf8(bulk.bytes)
            .unwrap()
            .contains("{time: 2/4}")
    );

    let pdf = app
        .get(
            &format!("/songs/{id}/export/pdf?chord_mode=inline&show_notes=true&lang=pt-BR"),
            &user,
        )
        .await;
    assert_eq!(pdf.status, StatusCode::OK, "{}", pdf.body);
    assert!(pdf.bytes.starts_with(b"%PDF"));
    assert!(
        pdf.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("song-")
    );

    // Without a plan there are no PDFs; a paid plan exports them, and
    // advanced options need a bigger plan.
    app.enforce_billing().await;
    let free = app
        .get(&format!("/songs/{id}/export/pdf?font_scale=150"), &user)
        .await;
    assert_eq!(free.code(), "FEATURE_NOT_IN_PLAN");
    assert_eq!(free.body["meta"]["feature"], "pdf_export");
    let (_, admin) = app.user("exportadmin", Role::Admin).await;
    app.put(
        &format!("/admin/users/{user_id}/subscription"),
        &admin,
        json!({ "plan_code": "basic", "days": 30 }),
    )
    .await;
    let advanced = app
        .get(&format!("/songs/{id}/export/pdf?columns=2"), &user)
        .await;
    assert_eq!(advanced.code(), "FEATURE_NOT_IN_PLAN");
    assert_eq!(advanced.body["meta"]["feature"], "advanced_pdf");
    let basic = app
        .get(&format!("/songs/{id}/export/pdf?font_scale=150"), &user)
        .await;
    assert_eq!(basic.status, StatusCode::OK);
}

#[tokio::test]
async fn band_song_exports_follow_the_band_pdf_permission() {
    let app = app!();
    let (_, owner) = app.user("pdfowner", Role::User).await;
    let (_, member) = app.user("pdfmember", Role::User).await;
    let (_, outsider) = app.user("pdfoutsider", Role::User).await;
    let band = app.band(&owner, "PDF Band").await;
    app.join_band(&owner, &member, &band, None).await;
    let setlist = app.setlist(&owner, "Set", Some(&band)).await;
    let song = app.song_id(&owner, "A", "Banda").await;
    app.add_to_setlist(&owner, &setlist, &song).await;
    let band_song = app
        .get(&format!("/setlists/{setlist}/items"), &owner)
        .await
        .body[0]["song"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        app.get(&format!("/songs/{band_song}/export/pdf"), &member)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        app.get(&format!("/songs/{band_song}/export/pdf"), &outsider)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    app.put(
        &format!("/bands/{band}/permissions"),
        &owner,
        json!({ "permissions": [{ "role": "member", "permission": "export_pdf", "allowed": false }] }),
    )
    .await;
    assert_eq!(
        app.get(&format!("/songs/{band_song}/export/chordpro"), &member)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn chordpro_files_are_previewed_and_imported() {
    let app = app!();
    let (_, user) = app.user("importer", Role::User).await;
    let content = "\u{FEFF}{title: Garota de Ipanema}\r\n{artist: Tom Jobim}\r\n{key: F}\r\n{tempo: 128}\r\n{image: x.png}\r\n{soc}\r\n[F]Olha que [Nope]coisa\r\n{eoc}\r\n";

    let preview = app
        .post(
            "/songs/import/chordpro?dry_run=true",
            &user,
            json!({ "content": content }),
        )
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert_eq!(preview.body["title"], "Garota de Ipanema");
    assert_eq!(preview.body["artist_name"], "Tom Jobim");
    assert_eq!(preview.body["tonality"], "F");
    assert_eq!(preview.body["tempo"], 128);
    assert_eq!(preview.body["lyrics"], "{soc}\n[F]Olha que coisa\n{eoc}");
    let codes: Vec<&str> = preview.body["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"directive_dropped") && codes.contains(&"invalid_chord"));
    // Nothing saved yet.
    assert_eq!(
        app.get("/songs", &user).await.body["meta"]["total_items"],
        0
    );

    let imported = app
        .post(
            "/songs/import/chordpro",
            &user,
            json!({ "content": content }),
        )
        .await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    assert_eq!(imported.body["title"], "Garota de Ipanema");
    assert_eq!(imported.body["tempo"], 128);
    let artists = app.get("/artists", &user).await;
    assert_eq!(artists.body["data"][0]["name"], "Tom Jobim");

    let again = app
        .post(
            "/songs/import/chordpro",
            &user,
            json!({ "content": content }),
        )
        .await;
    assert_eq!(again.code(), "ALREADY_EXISTS");
    let renamed = app
        .post(
            "/songs/import/chordpro",
            &user,
            json!({ "content": content, "title": "Outra", "artist_name": "tom jobim" }),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::CREATED);
    // The existing artist was reused (case-insensitive).
    assert_eq!(
        app.get("/artists", &user).await.body["meta"]["total_items"],
        1
    );

    let invalid = app
        .post(
            "/songs/import/chordpro",
            &user,
            json!({ "content": "[G]sem título" }),
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid.code(), "CHORDPRO_INVALID");
    assert_eq!(invalid.body["meta"]["reason"], "missing_title");
    let long_line = app
        .post(
            "/songs/import/chordpro?dry_run=true",
            &user,
            json!({ "content": format!("{{title: x}}\n{}", "a".repeat(501)) }),
        )
        .await;
    assert_eq!(long_line.body["meta"]["reason"], "line_too_long");
    assert_eq!(long_line.body["meta"]["line"], 2);
    let huge = app
        .post(
            "/songs/import/chordpro?dry_run=true",
            &user,
            json!({ "content": "a".repeat(64 * 1024 + 1) }),
        )
        .await;
    assert_eq!(huge.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(huge.code(), "CHORDPRO_TOO_LARGE");
    let no_artist = app
        .post(
            "/songs/import/chordpro",
            &user,
            json!({ "content": "{title: Sem artista}\nla" }),
        )
        .await;
    assert_eq!(no_artist.code(), "VALIDATION_ERROR");

    app.enforce_billing().await;
    let gated = app
        .post(
            "/songs/import/chordpro?dry_run=true",
            &user,
            json!({ "content": content }),
        )
        .await;
    assert_eq!(gated.code(), "FEATURE_NOT_IN_PLAN");
}

#[tokio::test]
async fn pins_are_personal_bounded_and_ordered() {
    let app = app!();
    let (_, user) = app.user("pinner", Role::User).await;
    let (_, other) = app.user("notpinner", Role::User).await;
    let song = app.song_id(&user, "A", "Fixada").await;
    let setlist = app.setlist(&user, "Fixo", None).await;
    let band = app.band(&user, "Pinned Band").await;
    let foreign = app.song_id(&other, "B", "Alheia").await;

    for (item_type, item_id) in [("song", &song), ("setlist", &setlist), ("band", &band)] {
        let pinned = app
            .put(
                "/users/me/pins",
                &user,
                json!({ "item_type": item_type, "item_id": item_id }),
            )
            .await;
        assert_eq!(pinned.status, StatusCode::NO_CONTENT, "{}", pinned.body);
    }
    // Idempotent.
    assert_eq!(
        app.put(
            "/users/me/pins",
            &user,
            json!({ "item_type": "song", "item_id": song })
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let denied = app
        .put(
            "/users/me/pins",
            &user,
            json!({ "item_type": "song", "item_id": foreign }),
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);

    let pins = app.get("/users/me/pins", &user).await;
    assert_eq!(pins.status, StatusCode::OK, "{}", pins.body);
    let list = pins.body.as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["item_type"], "song");
    assert_eq!(list[0]["title"], "Fixada");
    assert_eq!(list[0]["subtitle"], "A");
    assert_eq!(list[0]["href_hint"], format!("/dashboard/songs/{song}"));
    // `/users/me/pins` doesn't shadow the users routes.
    assert_eq!(app.get("/users/me", &user).await.status, StatusCode::OK);

    // `is_pinned` on lists and details.
    assert_eq!(
        app.get("/songs", &user).await.body["data"][0]["is_pinned"],
        true
    );
    assert_eq!(
        app.get(&format!("/setlists/{setlist}"), &user).await.body["is_pinned"],
        true
    );
    assert_eq!(app.get("/bands", &user).await.body[0]["is_pinned"], true);
    assert_eq!(
        app.get("/songs", &other).await.body["data"][0]["is_pinned"],
        false
    );

    let reordered = app
        .put(
            "/users/me/pins/order",
            &user,
            json!({ "items": [{ "item_type": "band", "item_id": band }, { "item_type": "song", "item_id": song }] }),
        )
        .await;
    assert_eq!(reordered.status, StatusCode::NO_CONTENT);
    let pins = app.get("/users/me/pins", &user).await;
    let order: Vec<&str> = pins
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["item_type"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["band", "song", "setlist"]);

    // Trashed items drop out silently.
    app.delete(&format!("/songs/{song}"), &user).await;
    assert_eq!(
        app.get("/users/me/pins", &user)
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        app.delete(&format!("/users/me/pins/setlist/{setlist}"), &user)
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    // At most 12.
    for i in 0..11 {
        let id = app.setlist(&user, &format!("Extra {i}"), None).await;
        app.put(
            "/users/me/pins",
            &user,
            json!({ "item_type": "setlist", "item_id": id }),
        )
        .await;
    }
    let extra = app.setlist(&user, "Extra final", None).await;
    let full = app
        .put(
            "/users/me/pins",
            &user,
            json!({ "item_type": "setlist", "item_id": extra }),
        )
        .await;
    assert_eq!(full.code(), "QUOTA_EXCEEDED");
    assert_eq!(full.body["meta"]["resource"], "pins");
    assert_eq!(full.body["meta"]["limit"], 12);
}

#[tokio::test]
async fn songs_show_their_artist_and_the_setlists_that_contain_them() {
    let app = app!();
    let (_, owner) = app.user("whereowner", Role::User).await;
    let (_, member) = app.user("wheremember", Role::User).await;
    let (_, outsider) = app.user("whereoutsider", Role::User).await;

    // Personal song: artist name on create, get and list.
    let artist = app.artist(&owner, "Compositor").await;
    let created = app.song(&owner, &artist, "Canção").await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["artist_name"], "Compositor");
    let song = created.body["id"].as_str().unwrap().to_string();
    let detail = app.get(&format!("/songs/{song}"), &owner).await;
    assert_eq!(detail.body["artist_name"], "Compositor");
    assert_eq!(detail.body["artist_id"], artist.as_str());
    assert_eq!(
        app.get("/songs", &owner).await.body["data"][0]["artist_name"],
        "Compositor"
    );

    let personal = app.setlist(&owner, "Bar", None).await;
    let _unrelated = app.setlist(&owner, "Vazia", None).await;
    let trashed = app.setlist(&owner, "Antiga", None).await;
    app.add_to_setlist(&owner, &personal, &song).await;
    app.add_to_setlist(&owner, &trashed, &song).await;
    assert_eq!(
        app.delete(&format!("/setlists/{trashed}"), &owner)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let used = app.get(&format!("/songs/{song}/setlists"), &owner).await;
    assert_eq!(used.status, StatusCode::OK, "{}", used.body);
    assert_eq!(
        used.body,
        json!([{ "id": personal, "title": "Bar", "is_repertoire": false, "band_id": null, "band_name": null, "position": 1 }])
    );
    assert_eq!(
        app.get(&format!("/songs/{song}/setlists"), &outsider)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // Band copy: in the band setlist and the repertoire, visible to members.
    let band = app.band(&owner, "Where Band").await;
    app.join_band(&owner, &member, &band, None).await;
    let repertoire = app.get(&format!("/bands/{band}"), &owner).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();
    let show = app.setlist(&owner, "Show", Some(&band)).await;
    app.add_to_setlist(&owner, &show, &song).await;
    let band_song = app
        .get(&format!("/setlists/{show}/items"), &owner)
        .await
        .body[0]["song"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let band_detail = app.get(&format!("/songs/{band_song}"), &member).await;
    assert_eq!(band_detail.status, StatusCode::OK, "{}", band_detail.body);
    assert_eq!(band_detail.body["artist_name"], "Compositor");
    let band_used = app
        .get(&format!("/songs/{band_song}/setlists"), &member)
        .await;
    let entries = band_used.body.as_array().unwrap();
    assert_eq!(entries.len(), 2, "{}", band_used.body);
    assert_eq!(entries[0]["id"], repertoire.as_str());
    assert_eq!(entries[0]["is_repertoire"], true);
    assert_eq!(entries[0]["band_name"], "Where Band");
    assert_eq!(entries[1]["id"], show.as_str());
    assert_eq!(entries[1]["band_id"], band.as_str());
    // The personal song isn't counted in band setlists.
    let used = app.get(&format!("/songs/{song}/setlists"), &owner).await;
    assert_eq!(used.body.as_array().unwrap().len(), 1);

    // The band's artist is readable by members (not by outsiders), and
    // stays read-only.
    let band_artist = band_detail.body["artist_id"].as_str().unwrap();
    let read = app.get(&format!("/artists/{band_artist}"), &member).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["name"], "Compositor");
    assert_eq!(read.body["band_id"], band.as_str());
    assert_eq!(
        app.get(&format!("/artists/{band_artist}"), &outsider)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_ne!(
        app.patch(
            &format!("/artists/{band_artist}"),
            &member,
            json!({ "name": "Outro" }),
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        app.get(&format!("/artists/{artist}"), &member).await.status,
        StatusCode::NOT_FOUND,
        "personal artists stay private"
    );

    // Pins flag the repertoire.
    for setlist in [&repertoire, &show] {
        let pinned = app
            .put(
                "/users/me/pins",
                &member,
                json!({ "item_type": "setlist", "item_id": setlist }),
            )
            .await;
        assert_eq!(pinned.status, StatusCode::NO_CONTENT, "{}", pinned.body);
    }
    let pins = app.get("/users/me/pins", &member).await;
    let flags: Vec<(String, bool)> = pins
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["item_id"].as_str().unwrap().to_string(),
                p["is_repertoire"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        vec![(repertoire.clone(), true), (show.clone(), false)]
    );
}
