//! Content security fixes: public DTOs, share takedowns reaching gigs,
//! duplicating band setlists, scoped setlist reads, payload bounds,
//! pagination clamps, plan gates and PDF limits.

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};
use setlyst_api::models::{quota::QuotaOverrides, user::Role};

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn public_pages_only_expose_performance_data() {
    let app = app!();
    let (_, user) = app.user("publisher", Role::User).await;
    let artist = app.artist(&user, "Artista").await;
    let song = app
        .post(
            "/songs",
            &user,
            json!({ "title": "Pública", "artist_id": artist, "tags": ["segredo"], "energy": 2,
                    "links": [{ "url": "https://youtu.be/x" }] }),
        )
        .await;
    let song_id = song.body["id"].as_str().unwrap().to_string();
    let setlist = app.setlist(&user, "Aberta", None).await;
    app.add_to_setlist(&user, &setlist, &song_id).await;
    app.post(
        &format!("/setlists/{setlist}/breaks"),
        &user,
        json!({ "label": "Pausa", "duration_minutes": 10 }),
    )
    .await;
    let token = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await
        .body["share_token"]
        .as_str()
        .unwrap()
        .to_string();

    let public = app
        .request(
            Method::GET,
            &format!("/public/setlists/{token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.status, StatusCode::OK);
    let mut song_keys = keys(&public.body["songs"][0]);
    song_keys.sort();
    assert_eq!(
        song_keys,
        vec![
            "artist_name",
            "capo",
            "duration",
            "energy",
            "links",
            "lyrics",
            "position",
            "tempo",
            "time_signature",
            "title",
            "tonality"
        ]
    );
    let mut marker_keys = keys(&public.body["markers"][0]);
    marker_keys.sort();
    assert_eq!(
        marker_keys,
        vec!["duration_minutes", "label", "marker_type", "position"]
    );
    let text = public.body.to_string();
    for leak in [
        "user_id",
        "band_id",
        "segredo",
        "updated_by",
        "created_at",
        "publisher",
        &song_id,
    ] {
        assert!(!text.contains(leak), "{leak} leaked: {text}");
    }
}

#[tokio::test]
async fn a_takedown_also_hides_the_setlist_from_public_gigs() {
    let app = app!();
    let (_, moderator) = app.user("warden", Role::Moderator).await;
    let (_, user) = app.user("gigsharer", Role::User).await;
    let setlist = app.setlist(&user, "Takedown", None).await;
    let gig = app
        .post(
            "/gigs",
            &user,
            json!({ "venue": "Clube", "scheduled_at": "2030-03-01T22:00:00", "setlist_id": setlist }),
        )
        .await;
    let gig_id = gig.body["id"].as_str().unwrap().to_string();
    let gig_token = app
        .post(&format!("/gigs/{gig_id}/share"), &user, json!({}))
        .await
        .body["share_token"]
        .as_str()
        .unwrap()
        .to_string();
    let public = app
        .request(
            Method::GET,
            &format!("/public/gigs/{gig_token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.body["setlist"]["title"], "Takedown");

    // Locked sharing (even without a link of its own) keeps the setlist
    // off the gig page.
    sqlx::query("UPDATE setlists SET share_locked_at = NOW() WHERE id = $1::uuid")
        .bind(&setlist)
        .execute(&app.pool)
        .await
        .unwrap();
    let public = app
        .request(
            Method::GET,
            &format!("/public/gigs/{gig_token}"),
            None,
            None,
        )
        .await;
    assert_eq!(public.status, StatusCode::OK);
    assert!(public.body["setlist"].is_null(), "{}", public.body);

    // A staff takedown of the setlist also kills the gig's link.
    sqlx::query("UPDATE setlists SET share_locked_at = NULL WHERE id = $1::uuid")
        .bind(&setlist)
        .execute(&app.pool)
        .await
        .unwrap();
    let revoked = app
        .post(
            &format!("/admin/setlists/{setlist}/share/revoke"),
            &moderator,
            json!({ "reason": "Direitos autorais" }),
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    let gone = app
        .request(
            Method::GET,
            &format!("/public/gigs/{gig_token}"),
            None,
            None,
        )
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);

    // A trashed setlist isn't shown either.
    let token = app
        .post(&format!("/gigs/{gig_id}/share"), &user, json!({}))
        .await
        .body["share_token"]
        .as_str()
        .unwrap()
        .to_string();
    app.post(
        &format!("/admin/setlists/{setlist}/share/unlock"),
        &moderator,
        json!({}),
    )
    .await;
    app.delete(&format!("/setlists/{setlist}"), &user).await;
    let public = app
        .request(Method::GET, &format!("/public/gigs/{token}"), None, None)
        .await;
    assert!(public.body["setlist"].is_null());
}

#[tokio::test]
async fn duplicating_a_band_setlist_forks_songs_into_the_callers_library() {
    let app = app!();
    let (_, owner) = app.user("dupowner", Role::User).await;
    let (member_id, member) = app.user("dupmember", Role::User).await;
    let band = app.band(&owner, "Dup Band").await;
    app.join_band(&owner, &member, &band, None).await;
    let setlist = app.setlist(&owner, "Band Set", Some(&band)).await;
    for title in ["Um", "Dois", "Três"] {
        let song = app.song_id(&owner, "Autor", title).await;
        app.add_to_setlist(&owner, &setlist, &song).await;
    }
    // The member already has "Um" by the same artist: it is reused.
    let existing = app.song_id(&member, "autor", "um").await;

    let copy = app
        .post(
            &format!("/setlists/{setlist}/duplicate"),
            &member,
            json!({}),
        )
        .await;
    assert_eq!(copy.status, StatusCode::CREATED, "{}", copy.body);
    assert_eq!(copy.body["skipped_band_songs"], 0);
    assert!(copy.body["band_id"].is_null());
    let copy_id = copy.body["id"].as_str().unwrap().to_string();
    let items = app
        .get(&format!("/setlists/{copy_id}/items"), &member)
        .await;
    let songs: Vec<&Value> = items
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["item_type"] == "song")
        .map(|i| &i["song"])
        .collect();
    assert_eq!(songs.len(), 3);
    assert!(songs.iter().all(|s| s["band_id"].is_null()));
    assert!(songs.iter().all(|s| s["user_id"] == member_id.to_string()));
    assert_eq!(songs[0]["id"], existing.as_str());
    // They are now the member's own songs.
    assert_eq!(
        app.get("/songs", &member).await.body["meta"]["total_items"],
        3
    );

    // Leaving the band keeps the member's copies, but never the band's.
    let (third_id, third) = app.user("dupthird", Role::User).await;
    app.join_band(&owner, &third, &band, None).await;
    app.set_quota(
        third_id,
        QuotaOverrides {
            songs: Some(1),
            ..Default::default()
        },
    )
    .await;
    let partial = app
        .post(&format!("/setlists/{setlist}/duplicate"), &third, json!({}))
        .await;
    assert_eq!(partial.status, StatusCode::CREATED, "{}", partial.body);
    assert_eq!(partial.body["skipped_band_songs"], 2);
    assert_eq!(partial.body["song_count"], 1);
}

#[tokio::test]
async fn personal_setlists_never_show_band_songs() {
    let app = app!();
    let (_, owner) = app.user("scopeowner", Role::User).await;
    let band = app.band(&owner, "Scope Band").await;
    let band_setlist = app.setlist(&owner, "Band", Some(&band)).await;
    let song = app.song_id(&owner, "A", "Original").await;
    app.add_to_setlist(&owner, &band_setlist, &song).await;
    let band_song = app
        .get(&format!("/setlists/{band_setlist}/items"), &owner)
        .await
        .body[0]["song"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A stale row (as an old duplicate could have left) is ignored.
    let personal = app.setlist(&owner, "Pessoal", None).await;
    sqlx::query(
        "INSERT INTO setlist_songs (setlist_id, song_id, position) VALUES ($1::uuid, $2::uuid, 1)",
    )
    .bind(&personal)
    .bind(&band_song)
    .execute(&app.pool)
    .await
    .unwrap();
    assert!(app.setlist_titles(&owner, &personal).await.is_empty());
    let songs = app
        .get(&format!("/setlists/{personal}/songs"), &owner)
        .await;
    assert_eq!(songs.body["meta"]["total_items"], 0);
    assert_eq!(
        app.get(&format!("/setlists/{personal}"), &owner).await.body["song_count"],
        0
    );
    // Nor can a band copy be added to a personal setlist.
    let refused = app
        .post(
            &format!("/setlists/{personal}/songs"),
            &owner,
            json!({ "song_id": band_song }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn payload_lists_are_bounded() {
    let app = app!();
    let (_, user) = app.user("bounded", Role::User).await;
    let setlist = app.setlist(&user, "Grande", None).await;
    let band = app.band(&user, "Bounded Band").await;

    let items: Vec<Value> = (0..1001)
        .map(|_| json!({ "item_type": "song", "id": uuid::Uuid::new_v4() }))
        .collect();
    let too_many = app
        .patch(
            &format!("/setlists/{setlist}/items/reorder"),
            &user,
            json!({ "items": items }),
        )
        .await;
    assert_eq!(too_many.code(), "VALIDATION_ERROR");
    let ids: Vec<uuid::Uuid> = (0..1001).map(|_| uuid::Uuid::new_v4()).collect();
    let too_many = app
        .patch(
            &format!("/setlists/{setlist}/songs/reorder"),
            &user,
            json!({ "song_ids": ids }),
        )
        .await;
    assert_eq!(too_many.code(), "VALIDATION_ERROR");
    let permissions: Vec<Value> = (0..21)
        .map(|_| json!({ "role": "member", "permission": "export_pdf", "allowed": true }))
        .collect();
    let too_many = app
        .put(
            &format!("/bands/{band}/permissions"),
            &user,
            json!({ "permissions": permissions }),
        )
        .await;
    assert_eq!(too_many.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn content_listings_clamp_huge_pages() {
    let app = app!();
    let (_, user) = app.user("paginator", Role::User).await;
    let band = app.band(&user, "Page Band").await;
    for path in [
        "/songs?page=9223372036854775807".to_string(),
        "/artists?page=9223372036854775807".to_string(),
        "/setlists?page=9223372036854775807".to_string(),
        "/gigs?page=9223372036854775807".to_string(),
        "/tours?page=9223372036854775807".to_string(),
        "/trash?page=9223372036854775807".to_string(),
        format!("/bands/{band}/setlists?page=9223372036854775807"),
        format!("/bands/{band}/gigs?page=9223372036854775807"),
        format!("/bands/{band}/tours?page=9223372036854775807"),
        format!("/bands/{band}/suggestions?page=9223372036854775807"),
        format!("/bands/{band}/repertoire?page=9223372036854775807"),
    ] {
        let response = app.get(&path, &user).await;
        assert_eq!(response.status, StatusCode::OK, "{path}: {}", response.body);
        assert_eq!(response.body["meta"]["current_page"], 100_000, "{path}");
    }
}

#[tokio::test]
async fn plans_gate_bands_sharing_and_advanced_pdfs() {
    let app = app!();
    let (_, admin) = app.user("planadmin", Role::Admin).await;
    let (_, user) = app.user("freeuser", Role::User).await;
    let setlist = app.setlist(&user, "Livre", None).await;
    app.enforce_billing().await;

    let band = app
        .post("/bands", &user, json!({ "name": "No Plan" }))
        .await;
    assert_eq!(band.code(), "FEATURE_NOT_IN_PLAN");
    assert_eq!(band.body["meta"]["feature"], "create_bands");
    let share = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await;
    assert_eq!(share.code(), "FEATURE_NOT_IN_PLAN");
    let advanced = app
        .get(
            &format!("/setlists/{setlist}/export/pdf?include_lyrics=true"),
            &user,
        )
        .await;
    assert_eq!(advanced.code(), "FEATURE_NOT_IN_PLAN");
    let basic = app
        .get(
            &format!("/setlists/{setlist}/export/pdf?compact=true"),
            &user,
        )
        .await;
    assert_eq!(basic.status, StatusCode::OK);
    // Admins always have every feature.
    assert_eq!(
        app.post("/bands", &admin, json!({ "name": "Admin Band" }))
            .await
            .status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn public_pdf_exports_are_rate_limited_per_client() {
    let app = app!();
    let (_, user) = app.user("pdfsharer", Role::User).await;
    let setlist = app.setlist(&user, "Rate", None).await;
    let token = app
        .post(&format!("/setlists/{setlist}/share"), &user, json!({}))
        .await
        .body["share_token"]
        .as_str()
        .unwrap()
        .to_string();

    let first = app
        .request_with_headers(
            Method::GET,
            &format!("/public/setlists/{token}/export/pdf?columns=2"),
            None,
            None,
            &[("x-forwarded-for", "198.51.100.7")],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK);
    assert!(first.bytes.starts_with(b"%PDF"));

    // Quick misses from the same client exhaust its burst.
    let mut statuses = Vec::new();
    for _ in 0..10 {
        let response = app
            .request_with_headers(
                Method::GET,
                "/public/setlists/not-a-token/export/pdf",
                None,
                None,
                &[("x-forwarded-for", "198.51.100.7")],
            )
            .await;
        statuses.push(response.status);
    }
    assert!(statuses.contains(&StatusCode::NOT_FOUND), "{statuses:?}");
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    // Another client isn't affected.
    let other = app
        .request(
            Method::GET,
            &format!("/public/setlists/{token}/export/pdf"),
            None,
            None,
        )
        .await;
    assert_eq!(other.status, StatusCode::OK);
}
