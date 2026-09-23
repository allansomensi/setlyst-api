//! Tours: CRUD, gigs in a tour, scope rules, plans and quotas.

mod common;

use axum::http::StatusCode;
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
async fn personal_tours_group_gigs_with_stats() {
    let app = app!();
    let (_, user) = app.user("roadie", Role::User).await;

    let bad = app
        .post(
            "/tours",
            &user,
            json!({ "name": "Ao contrário", "start_date": "2030-02-10", "end_date": "2030-02-01" }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.code(), "VALIDATION_ERROR");

    let tour = app
        .post(
            "/tours",
            &user,
            json!({ "name": "Turnê de Verão", "description": "Litoral", "start_date": "2030-01-01", "end_date": "2030-02-28" }),
        )
        .await;
    assert_eq!(tour.status, StatusCode::CREATED, "{}", tour.body);
    let tour_id = tour.body["id"].as_str().unwrap().to_string();
    assert_eq!(tour.body["gig_count"], 0);
    assert_eq!(tour.body["owner_username"], "roadie");

    let past = app
        .post(
            "/tours",
            &user,
            json!({ "name": "Antiga", "start_date": "2020-01-01", "end_date": "2020-01-10" }),
        )
        .await;
    assert_eq!(past.status, StatusCode::CREATED);

    let song = app.song_id(&user, "Artista", "Abertura").await;
    let setlist = app.setlist(&user, "Set", None).await;
    app.add_to_setlist(&user, &setlist, &song).await;

    let first = app
        .post(
            "/gigs",
            &user,
            json!({ "venue": "Praia", "scheduled_at": "2030-01-10T21:00:00", "tour_id": tour_id, "setlist_id": setlist }),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["tour_id"], tour_id.as_str());
    let second = app
        .post(
            "/gigs",
            &user,
            json!({ "venue": "Serra", "scheduled_at": "2030-02-10T21:00:00", "status": "cancelled" }),
        )
        .await;
    let second_id = second.body["id"].as_str().unwrap().to_string();
    let joined = app
        .patch(
            &format!("/gigs/{second_id}"),
            &user,
            json!({ "tour_id": tour_id, "location": "Centro" }),
        )
        .await;
    assert_eq!(joined.status, StatusCode::OK, "{}", joined.body);

    let gig = app.get(&format!("/gigs/{second_id}"), &user).await;
    assert_eq!(gig.body["tour_name"], "Turnê de Verão");
    assert_eq!(gig.body["location"], "Centro");

    let upcoming = app.get("/tours", &user).await;
    assert_eq!(upcoming.body["meta"]["total_items"], 1);
    assert_eq!(upcoming.body["data"][0]["gig_count"], 2);
    let past_list = app.get("/tours?status=past", &user).await;
    assert_eq!(past_list.body["data"][0]["name"], "Antiga");
    assert_eq!(
        app.get("/tours?status=all", &user).await.body["meta"]["total_items"],
        2
    );

    let detail = app.get(&format!("/tours/{tour_id}"), &user).await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    assert_eq!(detail.body["name"], "Turnê de Verão");
    let gigs = detail.body["gigs"].as_array().unwrap();
    assert_eq!(gigs.len(), 2);
    assert_eq!(gigs[0]["venue"], "Praia");
    assert_eq!(gigs[0]["setlist"]["title"], "Set");
    assert_eq!(gigs[0]["setlist"]["is_repertoire"], false);
    assert_eq!(gigs[0]["setlist"]["song_count"], 1);
    assert!(gigs[1]["setlist"].is_null());
    assert_eq!(detail.body["stats"]["total_gigs"], 2);
    assert_eq!(detail.body["stats"]["confirmed"], 1);
    assert_eq!(detail.body["stats"]["cancelled"], 1);
    assert!(detail.body["next_gig_at"].is_string());

    let filtered = app.get(&format!("/gigs?tour_id={tour_id}"), &user).await;
    assert_eq!(filtered.body["meta"]["total_items"], 2);

    // Clearable fields.
    let cleared = app
        .patch(
            &format!("/gigs/{second_id}"),
            &user,
            json!({ "tour_id": null, "location": null, "notes": null }),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK);
    let gig = app.get(&format!("/gigs/{second_id}"), &user).await;
    assert!(gig.body["tour_id"].is_null() && gig.body["location"].is_null());

    let updated = app
        .patch(
            &format!("/tours/{tour_id}"),
            &user,
            json!({ "name": "Verão 2030", "description": null }),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(updated.body["name"], "Verão 2030");
    assert!(updated.body["description"].is_null());
    let bad_dates = app
        .patch(
            &format!("/tours/{tour_id}"),
            &user,
            json!({ "end_date": "2029-01-01" }),
        )
        .await;
    assert_eq!(bad_dates.code(), "VALIDATION_ERROR");

    // Trashing the tour leaves its gigs, without a tour.
    assert_eq!(
        app.delete(&format!("/tours/{tour_id}"), &user).await.status,
        StatusCode::NO_CONTENT
    );
    let first_id = first.body["id"].as_str().unwrap();
    let gig = app.get(&format!("/gigs/{first_id}"), &user).await;
    assert!(gig.body["tour_id"].is_null());
    assert_eq!(
        app.get(&format!("/tours/{tour_id}"), &user).await.status,
        StatusCode::NOT_FOUND
    );
    // A trashed tour can't take gigs.
    let refused = app
        .patch(
            &format!("/gigs/{first_id}"),
            &user,
            json!({ "tour_id": tour_id }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    app.post(&format!("/trash/tour/{tour_id}/restore"), &user, json!({}))
        .await;
    let gig = app.get(&format!("/gigs/{first_id}"), &user).await;
    assert_eq!(gig.body["tour_id"], tour_id.as_str());
}

#[tokio::test]
async fn tours_stay_within_their_scope() {
    let app = app!();
    let (_, owner) = app.user("bandleader", Role::User).await;
    let (_, member) = app.user("sideman", Role::User).await;
    let (_, other) = app.user("outsider", Role::User).await;
    let band = app.band(&owner, "Tour Band").await;
    app.join_band(&owner, &member, &band, None).await;

    // Members can't manage setlists by default, so no tours either.
    let denied = app
        .post(
            "/tours",
            &member,
            json!({ "name": "Nope", "start_date": "2030-01-01", "end_date": "2030-01-02", "band_id": band }),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let band_tour = app
        .post(
            "/tours",
            &owner,
            json!({ "name": "Band Tour", "start_date": "2030-01-01", "end_date": "2030-01-31", "band_id": band }),
        )
        .await;
    assert_eq!(band_tour.status, StatusCode::CREATED, "{}", band_tour.body);
    let band_tour_id = band_tour.body["id"].as_str().unwrap().to_string();
    assert_eq!(band_tour.body["band_name"], "Tour Band");

    let listed = app.get(&format!("/bands/{band}/tours"), &member).await;
    assert_eq!(listed.body["meta"]["total_items"], 1);
    assert_eq!(
        app.get(&format!("/tours/{band_tour_id}"), &member)
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        app.get(&format!("/tours/{band_tour_id}"), &other)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get(&format!("/bands/{band}/tours"), &other)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Personal tour lists don't include band tours.
    assert_eq!(
        app.get("/tours", &owner).await.body["meta"]["total_items"],
        0
    );

    // A band tour can't hold a personal gig, and vice versa.
    let personal_gig = app
        .post(
            "/gigs",
            &owner,
            json!({ "venue": "Solo", "scheduled_at": "2030-01-05T20:00:00", "tour_id": band_tour_id }),
        )
        .await;
    assert_eq!(personal_gig.status, StatusCode::BAD_REQUEST);
    let other_tour = app
        .post(
            "/tours",
            &other,
            json!({ "name": "Alheia", "start_date": "2030-01-01", "end_date": "2030-01-02" }),
        )
        .await;
    let other_tour_id = other_tour.body["id"].as_str().unwrap();
    let foreign = app
        .post(
            "/gigs",
            &owner,
            json!({ "venue": "Solo", "scheduled_at": "2030-01-05T20:00:00", "tour_id": other_tour_id }),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::BAD_REQUEST);
    let repertoire = app.get(&format!("/bands/{band}"), &owner).await.body["repertoire_id"]
        .as_str()
        .unwrap()
        .to_string();
    let band_gig = app
        .post(
            "/gigs",
            &owner,
            json!({ "venue": "Arena", "scheduled_at": "2030-01-05T20:00:00", "band_id": band, "tour_id": band_tour_id, "setlist_id": repertoire }),
        )
        .await;
    assert_eq!(band_gig.status, StatusCode::CREATED, "{}", band_gig.body);
    // The repertoire is flagged so clients can show its translated name.
    let timeline = app.get(&format!("/tours/{band_tour_id}"), &member).await;
    assert_eq!(
        timeline.body["gigs"][0]["setlist"]["id"],
        repertoire.as_str()
    );
    assert_eq!(timeline.body["gigs"][0]["setlist"]["is_repertoire"], true);
    let band_gigs = app
        .get(
            &format!("/bands/{band}/gigs?tour_id={band_tour_id}"),
            &member,
        )
        .await;
    assert_eq!(band_gigs.body["meta"]["total_items"], 1);

    // The member can't edit or delete it.
    assert_eq!(
        app.delete(&format!("/tours/{band_tour_id}"), &member)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn tours_need_the_plan_feature_and_respect_quotas() {
    let app = app!();
    let (user_id, user) = app.user("planner", Role::User).await;
    app.set_quota(
        user_id,
        QuotaOverrides {
            tours: Some(1),
            ..Default::default()
        },
    )
    .await;
    let body = json!({ "name": "Uma", "start_date": "2030-01-01", "end_date": "2030-01-02" });
    assert_eq!(
        app.post("/tours", &user, body.clone()).await.status,
        StatusCode::CREATED
    );
    let over = app.post("/tours", &user, body.clone()).await;
    assert_eq!(over.code(), "QUOTA_EXCEEDED");
    assert_eq!(over.body["meta"]["resource"], "tours");

    app.enforce_billing().await;
    let gated = app.post("/tours", &user, body).await;
    assert_eq!(gated.status, StatusCode::FORBIDDEN);
    assert_eq!(gated.code(), "FEATURE_NOT_IN_PLAN");
    assert_eq!(gated.body["meta"]["feature"], "tours");
}

#[tokio::test]
async fn band_tours_survive_their_creator_account() {
    let app = app!();
    let (_, admin) = app.user("sysadmin", Role::Admin).await;
    let (_, owner) = app.user("keeper", Role::User).await;
    let (creator_id, creator) = app.user("leaver", Role::User).await;
    let band = app.band(&owner, "Lasting Band").await;
    app.join_band(&owner, &creator, &band, Some("moderator"))
        .await;

    let tour = app
        .post(
            "/tours",
            &creator,
            json!({ "name": "Legado", "start_date": "2030-01-01", "end_date": "2030-01-02", "band_id": band }),
        )
        .await;
    assert_eq!(tour.status, StatusCode::CREATED, "{}", tour.body);
    let tour_id = tour.body["id"].as_str().unwrap().to_string();
    // A trashed band tour survives too.
    let trashed = app
        .post(
            "/tours",
            &creator,
            json!({ "name": "Na lixeira", "start_date": "2030-01-01", "end_date": "2030-01-02", "band_id": band }),
        )
        .await;
    let trashed_id = trashed.body["id"].as_str().unwrap().to_string();
    app.delete(&format!("/tours/{trashed_id}"), &creator).await;

    let deleted = app.delete(&format!("/users/{creator_id}"), &admin).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);

    let detail = app.get(&format!("/tours/{tour_id}"), &owner).await;
    assert_eq!(detail.status, StatusCode::OK);
    assert_eq!(detail.body["owner_username"], "keeper");
    let trash = app
        .get(&format!("/trash?scope=band&band_id={band}"), &owner)
        .await;
    assert_eq!(trash.body["meta"]["total_items"], 1, "{}", trash.body);
}
