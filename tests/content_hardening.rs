//! Launch hardening: payload bounds (tags, bodies), quota races, backup
//! import limits, PDF size caps, band ownership integrity, band
//! invites and one-click unsubscribe.

mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::{Value, json};
use setlyst_api::models::{communication::Category, quota::QuotaOverrides, user::Role};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::spawn().await {
            Some(app) => app,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------
// Payload bounds
// ---------------------------------------------------------------------

#[tokio::test]
async fn oversized_tag_lists_are_refused_before_any_work() {
    let app = app!();
    let (_, token) = app.user("tagger", Role::User).await;
    let artist = app.artist(&token, "Tagged").await;

    let eleven: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
    let create = app
        .post(
            "/songs",
            &token,
            json!({ "title": "Too many", "artist_id": artist, "tags": eleven }),
        )
        .await;
    assert_eq!(create.status, StatusCode::BAD_REQUEST, "{}", create.body);
    assert_eq!(create.code(), "VALIDATION_ERROR");

    // A flood of distinct tags (what used to pin a worker for minutes)
    // answers at once, well inside the song body limit.
    let flood: Vec<String> = (0..60_000).map(|i| format!("t{i}")).collect();
    let started = std::time::Instant::now();
    let flooded = app
        .post(
            "/songs",
            &token,
            json!({ "title": "Flood", "artist_id": artist, "tags": flood }),
        )
        .await;
    assert_eq!(flooded.status, StatusCode::BAD_REQUEST, "{}", flooded.body);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    let song = app.song(&token, &artist, "Fine").await;
    let id = song.body["id"].as_str().unwrap();
    let eleven: Vec<String> = (0..11).map(|i| format!("tag{i}")).collect();
    let update = app
        .patch(&format!("/songs/{id}"), &token, json!({ "tags": eleven }))
        .await;
    assert_eq!(update.status, StatusCode::BAD_REQUEST, "{}", update.body);
    // Ten are fine.
    let ten: Vec<String> = (0..10).map(|i| format!("tag{i}")).collect();
    let update = app
        .patch(&format!("/songs/{id}"), &token, json!({ "tags": ten }))
        .await;
    assert_eq!(update.status, StatusCode::OK, "{}", update.body);
}

#[tokio::test]
async fn request_bodies_are_bounded_per_route() {
    let app = app!();
    let (_, token) = app.user("bodies", Role::User).await;

    // Ordinary routes: 256 KB.
    let big = json!({ "name": "x".repeat(300 * 1024) })
        .to_string()
        .into_bytes();
    let artist = app
        .request_raw(
            Method::POST,
            "/artists",
            Some(&token),
            "application/json",
            big,
        )
        .await;
    assert_eq!(artist.status, StatusCode::PAYLOAD_TOO_LARGE);

    // Songs take up to 1 MB (50 000 characters of lyrics, escaped): the
    // same size reaches validation instead.
    let artist_id = app.artist(&token, "Lyricist").await;
    let lyrics = "\\u00e9".repeat(49_000);
    let body = format!(r#"{{"title":"Long","artist_id":"{artist_id}","lyrics":"{lyrics}"}}"#);
    assert!(body.len() > 256 * 1024);
    let song = app
        .request_raw(
            Method::POST,
            "/songs",
            Some(&token),
            "application/json",
            body.into_bytes(),
        )
        .await;
    assert_eq!(song.status, StatusCode::CREATED, "{}", song.body);
}

// ---------------------------------------------------------------------
// Quotas under concurrency
// ---------------------------------------------------------------------

#[tokio::test]
async fn concurrent_creates_never_exceed_a_quota() {
    let app = app!();
    let (user_id, token) = app.user("racer", Role::User).await;
    app.set_quota(
        user_id,
        QuotaOverrides {
            songs: Some(3),
            setlists: Some(2),
            ..Default::default()
        },
    )
    .await;
    let artist = app.artist(&token, "Racers").await;

    let songs: Vec<(Method, String, Value)> = (0..12)
        .map(|i| {
            (
                Method::POST,
                "/songs".to_string(),
                json!({ "title": format!("Race {i}"), "artist_id": artist }),
            )
        })
        .collect();
    let responses = app.concurrent(Some(&token), songs).await;
    let created = responses
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    assert_eq!(created, 3, "exactly the quota is created");
    assert!(
        responses
            .iter()
            .filter(|r| r.status != StatusCode::CREATED)
            .all(|r| r.code() == "QUOTA_EXCEEDED")
    );

    let setlists: Vec<(Method, String, Value)> = (0..8)
        .map(|i| {
            (
                Method::POST,
                "/setlists".to_string(),
                json!({ "title": format!("Set {i}") }),
            )
        })
        .collect();
    let responses = app.concurrent(Some(&token), setlists).await;
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.status == StatusCode::CREATED)
            .count(),
        2
    );
    let (songs, setlists): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM songs WHERE user_id = $1),
                (SELECT COUNT(*) FROM setlists WHERE user_id = $1)",
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!((songs, setlists), (3, 2));
}

#[tokio::test]
async fn setlist_items_and_repertoire_markers_are_capped() {
    let app = app!();
    let (user_id, token) = app.user("marker", Role::User).await;
    app.set_quota(
        user_id,
        QuotaOverrides {
            setlist_items: Some(2),
            ..Default::default()
        },
    )
    .await;
    let setlist = app.setlist(&token, "Short", None).await;
    let blocks: Vec<(Method, String, Value)> = (0..6)
        .map(|i| {
            (
                Method::POST,
                format!("/setlists/{setlist}/blocks"),
                json!({ "name": format!("Block {i}") }),
            )
        })
        .collect();
    let responses = app.concurrent(Some(&token), blocks).await;
    assert_eq!(
        responses
            .iter()
            .filter(|r| r.status == StatusCode::CREATED)
            .count(),
        2
    );

    // A repertoire has no item quota for its songs, but its blocks and
    // breaks are bounded.
    let band = app.band(&token, "Markers").await;
    let repertoire = app.get(&format!("/bands/{band}"), &token).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();
    sqlx::query(
        "INSERT INTO setlist_markers (id, setlist_id, marker_type, label, position, created_at)
         SELECT gen_random_uuid(), $1, 'block', 'b' || g, g, NOW() FROM generate_series(1, $2) g",
    )
    .bind(Uuid::parse_str(&repertoire).unwrap())
    .bind(setlyst_api::database::repositories::setlist_repository::MAX_REPERTOIRE_MARKERS as i32)
    .execute(&app.pool)
    .await
    .unwrap();
    let over = app
        .post(
            &format!("/setlists/{repertoire}/breaks"),
            &token,
            json!({ "label": "One too many" }),
        )
        .await;
    assert_eq!(over.code(), "QUOTA_EXCEEDED", "{}", over.body);
    assert_eq!(over.body["meta"]["resource"], "repertoire_markers");
}

// ---------------------------------------------------------------------
// Backup import
// ---------------------------------------------------------------------

fn backup(songs: usize, tours: usize) -> Value {
    let artist = Uuid::new_v4();
    json!({
        "version": 2,
        "exported_at": "2026-09-01T00:00:00",
        "artists": [{ "id": artist, "name": "Imported" }],
        "songs": (0..songs).map(|i| json!({
            "id": Uuid::new_v4(), "title": format!("Song {i}"), "artist_id": artist,
            "tempo": null, "lyrics": null, "tonality": null, "genre": null, "duration": null,
            "tags": ["a", "b"],
        })).collect::<Vec<_>>(),
        "setlists": [],
        "gigs": [],
        "tours": (0..tours).map(|i| json!({
            "id": Uuid::new_v4(), "name": format!("Tour {i}"), "description": null,
            "start_date": "2026-10-01", "end_date": "2026-10-10",
        })).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn backup_import_checks_the_quota_before_writing_and_runs_one_at_a_time() {
    let app = app!();
    let (user_id, token) = app.user("importer", Role::User).await;
    app.set_quota(
        user_id,
        QuotaOverrides {
            songs: Some(2),
            ..Default::default()
        },
    )
    .await;

    let refused = app.post("/backup/import", &token, backup(3, 0)).await;
    assert_eq!(refused.code(), "QUOTA_EXCEEDED", "{}", refused.body);
    assert_eq!(refused.body["meta"]["resource"], "songs");
    let artists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(artists, 0, "nothing written");

    // Another import holding the account's import lock: the second one is
    // refused at once.
    let mut holder = app.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('backup_import:' || $1::text, 0))")
        .bind(user_id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let busy = app.post("/backup/import", &token, backup(1, 0)).await;
    assert_eq!(busy.status, StatusCode::CONFLICT, "{}", busy.body);
    assert_eq!(busy.code(), "IMPORT_IN_PROGRESS");
    holder.rollback().await.unwrap();

    let ok = app.post("/backup/import", &token, backup(2, 0)).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
    assert_eq!(ok.body["songs_imported"], 2);

    // Three imports an hour.
    let limited = app.post("/backup/import", &token, backup(0, 0)).await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.body
    );
    assert_eq!(limited.code(), "TOO_MANY_ATTEMPTS");
}

#[tokio::test]
async fn backup_import_validates_tags_positions_and_skips_tours_without_the_feature() {
    let app = app!();
    let (_, token) = app.user("validator", Role::User).await;

    let mut file = backup(1, 0);
    file["songs"][0]["tags"] = json!((0..11).map(|i| format!("t{i}")).collect::<Vec<_>>());
    let tags = app.post("/backup/import", &token, file).await;
    assert_eq!(tags.status, StatusCode::BAD_REQUEST, "{}", tags.body);

    let mut file = backup(1, 0);
    let song = file["songs"][0]["id"].clone();
    file["setlists"] = json!([{
        "id": Uuid::new_v4(), "title": "Far", "description": null,
        "songs": [{ "song_id": song, "position": i32::MAX }],
    }]);
    let positions = app.post("/backup/import", &token, file).await;
    assert_eq!(
        positions.status,
        StatusCode::BAD_REQUEST,
        "{}",
        positions.body
    );

    // Plans enforced, no plan: no `tours` feature.
    app.enforce_billing().await;
    let imported = app.post("/backup/import", &token, backup(1, 2)).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    assert_eq!(imported.body["tours_imported"], 0);
    assert_eq!(imported.body["skipped_tours"], 2);
}

// ---------------------------------------------------------------------
// PDF caps
// ---------------------------------------------------------------------

#[tokio::test]
async fn songbooks_are_capped_and_share_links_never_render_lyrics() {
    let app = app!();
    let (_, token) = app.user("songbook", Role::User).await;
    let artist = app.artist(&token, "Verbose").await;
    let setlist = app.setlist(&token, "Heavy", None).await;
    for i in 0..6 {
        let song = app
            .post(
                "/songs",
                &token,
                json!({ "title": format!("Long {i}"), "artist_id": artist, "lyrics": "la ".repeat(16_000) }),
            )
            .await;
        assert_eq!(song.status, StatusCode::CREATED, "{}", song.body);
        app.add_to_setlist(&token, &setlist, song.body["id"].as_str().unwrap())
            .await;
    }

    let songbook = app
        .get(
            &format!("/setlists/{setlist}/export/pdf?include_lyrics=true"),
            &token,
        )
        .await;
    assert_eq!(songbook.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(songbook.code(), "PDF_TOO_LARGE");
    assert_eq!(songbook.body["meta"]["reason"], "songbook");

    // The running order alone renders.
    let running_order = app
        .get(&format!("/setlists/{setlist}/export/pdf"), &token)
        .await;
    assert_eq!(running_order.status, StatusCode::OK);

    // The public link ignores the songbook options altogether.
    let share = app
        .post(&format!("/setlists/{setlist}/share"), &token, json!({}))
        .await;
    let share_token = share.body["share_token"].as_str().unwrap().to_string();
    let public = app
        .request(
            Method::GET,
            &format!(
                "/public/setlists/{share_token}/export/pdf?include_lyrics=true&page_break_per_song=true"
            ),
            None,
            None,
        )
        .await;
    assert_eq!(public.status, StatusCode::OK, "{}", public.body);
    assert_eq!(public.headers["content-type"], "application/pdf");
    // Rendered like the running order alone (a songbook of 96 000
    // characters would add dozens of pages).
    let (public_len, plain_len) = (public.bytes.len(), running_order.bytes.len());
    assert!(
        public_len.abs_diff(plain_len) < plain_len / 20,
        "public {public_len} bytes vs running order {plain_len} bytes"
    );
}

// ---------------------------------------------------------------------
// Band ownership and membership
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_band_never_ends_up_with_two_owners() {
    let app = app!();
    let (_, owner) = app.user("single.owner", Role::User).await;
    let (b_id, b) = app.user("heir.b", Role::User).await;
    let (c_id, c) = app.user("heir.c", Role::User).await;
    let band = app.band(&owner, "One Owner").await;
    app.join_band(&owner, &b, &band, None).await;
    app.join_band(&owner, &c, &band, None).await;

    let responses = app
        .concurrent(
            Some(&owner),
            vec![
                (
                    Method::POST,
                    format!("/bands/{band}/transfer-ownership"),
                    json!({ "new_owner_id": b_id }),
                ),
                (
                    Method::POST,
                    format!("/bands/{band}/transfer-ownership"),
                    json!({ "new_owner_id": c_id }),
                ),
            ],
        )
        .await;
    let succeeded = responses
        .iter()
        .filter(|r| r.status == StatusCode::OK)
        .count();
    assert_eq!(
        succeeded,
        1,
        "{:?}",
        responses.iter().map(|r| &r.body).collect::<Vec<_>>()
    );
    let owners: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM band_members WHERE band_id = $1::uuid AND role = 'owner'",
    )
    .bind(&band)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(owners, 1);

    // The database refuses a second owner outright.
    let second = sqlx::query(
        "UPDATE band_members SET role = 'owner' WHERE band_id = $1::uuid AND role <> 'owner'",
    )
    .bind(&band)
    .execute(&app.pool)
    .await;
    assert!(second.is_err(), "a second owner was accepted");
}

#[tokio::test]
async fn member_changes_respect_the_hierarchy_and_revoke_invites() {
    let app = app!();
    let (_, owner) = app.user("hier.owner", Role::User).await;
    let (admin_id, admin) = app.user("hier.admin", Role::User).await;
    let (_, other_admin) = app.user("hier.admin2", Role::User).await;
    let owner_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'hier.owner'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let band = app.band(&owner, "Hierarchy").await;
    app.join_band(&owner, &admin, &band, Some("admin")).await;
    app.join_band(&owner, &other_admin, &band, Some("admin"))
        .await;

    // An admin can't relabel the owner, nor another admin.
    let title = app
        .patch(
            &format!("/bands/{band}/members/{owner_id}/title"),
            &admin,
            json!({ "title": "Roadie" }),
        )
        .await;
    assert_eq!(title.status, StatusCode::FORBIDDEN, "{}", title.body);

    // The admin's invites die with their demotion.
    let invite = app
        .post(&format!("/bands/{band}/invites"), &admin, json!({}))
        .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    let demoted = app
        .patch(
            &format!("/bands/{band}/members/{admin_id}"),
            &owner,
            json!({ "role": "member" }),
        )
        .await;
    assert_eq!(demoted.status, StatusCode::OK, "{}", demoted.body);
    let (_, joiner) = app.user("hier.joiner", Role::User).await;
    let code = invite.body["code"].as_str().unwrap();
    let accepted = app
        .post(&format!("/invites/{code}/accept"), &joiner, json!({}))
        .await;
    assert_eq!(accepted.status, StatusCode::NOT_FOUND, "{}", accepted.body);
}

#[tokio::test]
async fn a_band_has_at_most_fifty_usable_invites() {
    let app = app!();
    let (_, owner) = app.user("inviter", Role::User).await;
    let band = app.band(&owner, "Invites").await;
    for _ in 0..50 {
        let invite = app
            .post(&format!("/bands/{band}/invites"), &owner, json!({}))
            .await;
        assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    }
    let over = app
        .post(&format!("/bands/{band}/invites"), &owner, json!({}))
        .await;
    assert_eq!(over.code(), "QUOTA_EXCEEDED", "{}", over.body);
    assert_eq!(over.body["meta"]["resource"], "band_invites");
}

#[tokio::test]
async fn duplicating_a_band_setlist_needs_the_export_permission() {
    let app = app!();
    let (_, owner) = app.user("dup.owner", Role::User).await;
    let (_, member) = app.user("dup.member", Role::User).await;
    let band = app.band(&owner, "No Copies").await;
    app.join_band(&owner, &member, &band, None).await;
    let setlist = app.setlist(&owner, "Band set", Some(&band)).await;
    let permissions = app
        .put(
            &format!("/bands/{band}/permissions"),
            &owner,
            json!({ "permissions": [
                { "role": "member", "permission": "export_pdf", "allowed": false }
            ] }),
        )
        .await;
    assert_eq!(
        permissions.status,
        StatusCode::NO_CONTENT,
        "{}",
        permissions.body
    );

    let copy = app
        .post(
            &format!("/setlists/{setlist}/duplicate"),
            &member,
            json!({}),
        )
        .await;
    assert_eq!(copy.status, StatusCode::FORBIDDEN, "{}", copy.body);
    let own = app
        .post(&format!("/setlists/{setlist}/duplicate"), &owner, json!({}))
        .await;
    assert_eq!(own.status, StatusCode::CREATED, "{}", own.body);
}

// ---------------------------------------------------------------------
// E-mail
// ---------------------------------------------------------------------

#[tokio::test]
async fn one_click_unsubscribe_accepts_a_form_post() {
    let app = app!();
    let (user_id, _) = app.user("oneclick", Role::User).await;
    let token = setlyst_api::email::unsubscribe::token(user_id, Category::Announcements);

    let response = app
        .request_raw(
            Method::POST,
            &format!("/public/email/unsubscribe/one-click?token={token}"),
            None,
            "application/x-www-form-urlencoded",
            b"List-Unsubscribe=One-Click".to_vec(),
        )
        .await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    assert_eq!(response.body["category"], "announcements");

    let (prefs, _) = app
        .state
        .user_prefs_repo
        .get_communication(user_id)
        .await
        .unwrap();
    assert!(!prefs.get(Category::Announcements).email);

    let forged = app
        .request_raw(
            Method::POST,
            "/public/email/unsubscribe/one-click?token=forged-token-value",
            None,
            "application/x-www-form-urlencoded",
            Vec::new(),
        )
        .await;
    assert_eq!(forged.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn codes_are_capped_per_address_and_sent_before_bulk_mail() {
    use setlyst_api::email::outbox::CODE_EMAILS_PER_ADDRESS_PER_DAY;
    use setlyst_api::email::{
        EmailTemplate, OutgoingEmail,
        outbox::{enqueue, enqueue_many},
        worker::process_batch,
    };
    let app = app!();
    // A code that can be sent to an arbitrary address (sign-up), unlike a
    // recovery code, which only ever goes to the account's own address
    // and is bounded by the issuer instead.
    let code = |n: i64| OutgoingEmail {
        user_id: None,
        to: "Victim@Example.com".into(),
        locale: "en".into(),
        template: EmailTemplate::EmailVerificationCode {
            username: "victim".into(),
            code: format!("{n:06}"),
            expires_minutes: 15,
        },
    };
    for n in 0..(CODE_EMAILS_PER_ADDRESS_PER_DAY + 3) {
        enqueue(&app.pool, &code(n)).await.unwrap();
    }
    let queued: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_outbox WHERE LOWER(to_email) = 'victim@example.com'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        queued, CODE_EMAILS_PER_ADDRESS_PER_DAY,
        "per-address cap on one-time codes"
    );

    // A large announcement queued first still goes out after the codes.
    let bulk: Vec<OutgoingEmail> = (0..40)
        .map(|i| OutgoingEmail {
            user_id: None,
            to: format!("fan{i}@example.com"),
            locale: "en".into(),
            template: EmailTemplate::Announcement {
                title: "News".into(),
                body: "Hello".into(),
                level: "info".into(),
                cta_label: None,
                cta_url: None,
            },
        })
        .collect();
    enqueue_many(&app.pool, &bulk).await.unwrap();
    sqlx::query(
        "UPDATE email_outbox SET scheduled_at = scheduled_at - INTERVAL '1 hour' WHERE template = 'announcement'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    process_batch(&app.pool, None, "https://setlyst.test")
        .await
        .unwrap();
    let pending_codes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM email_outbox WHERE template = 'email_verification_code' AND status = 'pending'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(pending_codes, 0, "codes are claimed first");
}

// ---------------------------------------------------------------------
// Verified e-mail gates (CONTRACTS §5)
// ---------------------------------------------------------------------

#[tokio::test]
async fn bands_sharing_logos_and_imports_need_a_verified_email() {
    let app = app!();
    let (user_id, token) = app.unverified_user("unproven", Role::User).await;
    let not_verified = |response: &common::TestResponse| {
        assert_eq!(response.status, StatusCode::FORBIDDEN, "{}", response.body);
        assert_eq!(response.code(), "EMAIL_NOT_VERIFIED", "{}", response.body);
    };

    // Band creation.
    not_verified(&app.post("/bands", &token, json!({ "name": "Nope" })).await);

    // Public sharing of setlists and gigs (their own content is fine).
    let setlist = app.setlist(&token, "Private", None).await;
    not_verified(
        &app.post(&format!("/setlists/{setlist}/share"), &token, json!({}))
            .await,
    );
    let gig = app
        .post(
            "/gigs",
            &token,
            json!({ "venue": "Bar", "scheduled_at": "2030-03-01T22:00:00" }),
        )
        .await;
    assert_eq!(gig.status, StatusCode::CREATED, "{}", gig.body);
    let gig = gig.body["id"].as_str().unwrap().to_string();
    not_verified(
        &app.post(&format!("/gigs/{gig}/share"), &token, json!({}))
            .await,
    );

    // Backup import (refused before counting against the hourly limit).
    for _ in 0..4 {
        not_verified(&app.post("/backup/import", &token, backup(1, 0)).await);
    }

    // A new logo, as an admin of someone else's band; other edits pass.
    let (_, owner) = app.user("band.owner", Role::User).await;
    let band = app.band(&owner, "Verified Band").await;
    app.join_band(&owner, &token, &band, Some("admin")).await;
    not_verified(
        &app.patch(
            &format!("/bands/{band}"),
            &token,
            json!({ "logo_url": "https://images.example.com/logo.png" }),
        )
        .await,
    );
    let renamed = app
        .patch(
            &format!("/bands/{band}"),
            &token,
            json!({ "description": "Still editable" }),
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);

    // Once the address is verified, everything goes through.
    app.set_verified_email(user_id, "unproven@example.com")
        .await;
    app.band(&token, "Now Allowed").await;
    let shared = app
        .post(&format!("/setlists/{setlist}/share"), &token, json!({}))
        .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.body);
    let shared = app
        .post(&format!("/gigs/{gig}/share"), &token, json!({}))
        .await;
    assert_eq!(shared.status, StatusCode::OK, "{}", shared.body);
    let imported = app.post("/backup/import", &token, backup(1, 0)).await;
    assert_eq!(imported.status, StatusCode::CREATED, "{}", imported.body);
    let logo = app
        .patch(
            &format!("/bands/{band}"),
            &token,
            json!({ "logo_url": "https://images.example.com/logo.png" }),
        )
        .await;
    assert_eq!(logo.status, StatusCode::OK, "{}", logo.body);
}
