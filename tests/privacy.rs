//! LGPD: the personal data export and the audit log retention.

mod common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::TestApp;
use setlyst_api::jobs::apply_audit_retention;
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
async fn the_personal_data_export_has_everything_but_secrets() {
    let app = app!();
    let (user_id, token) = app
        .registered_user("data.owner", "data.owner@example.com")
        .await;
    let (_, other) = app
        .registered_user("someone.else", "someone.else@example.com")
        .await;

    let export = app.get("/users/me/data-export", &token).await;
    assert_eq!(export.status, StatusCode::OK, "{}", export.body);
    let body = &export.body;
    assert_eq!(body["format"], "setlyst.personal-data");
    assert_eq!(body["account"]["id"], user_id.to_string());
    assert_eq!(body["account"]["username"], "data.owner");
    assert_eq!(body["account"]["email"], "data.owner@example.com");
    for key in body["account"].as_object().unwrap().keys() {
        assert!(
            !key.contains("password")
                && !key.contains("totp")
                && !key.contains("secret")
                && !key.ends_with("_by"),
            "secret column exported: {key}"
        );
    }
    assert!(body["preferences"].is_object());
    assert!(body["security_log"].as_array().is_some());
    assert!(body["payments"].as_array().unwrap().is_empty());
    let text = body.to_string();
    assert!(!text.contains("someone.else"), "another account leaked");

    // Each account only ever gets its own.
    let theirs = app.get("/users/me/data-export", &other).await;
    assert_eq!(theirs.body["account"]["username"], "someone.else");
    assert_eq!(
        app.get("/users/me/data-export", "not-a-token").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn access_records_are_stripped_after_six_months_and_entries_dropped_after_five_years() {
    let app = app!();
    let now = Utc::now().naive_utc();
    let insert = |id: Uuid, action: &'static str, label: Option<&'static str>, days: i64| {
        let pool = app.pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO audit_logs (id, action, target_type, target_label, metadata, ip_address, created_at)
                 VALUES ($1, $2, 'user', $3, '{}', '203.0.113.9', $4)",
            )
            .bind(id)
            .bind(action)
            .bind(label)
            .bind(now - Duration::days(days))
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    let (recent, old_failed, old_other, ancient) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    // Sign-up records: kept while the account exists (referral checks),
    // stripped once it's gone.
    let (user_id, _) = app
        .registered_user("still.here", "still.here@example.com")
        .await;
    let (live_signup, gone_signup) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, target) in [(live_signup, user_id), (gone_signup, Uuid::now_v7())] {
        sqlx::query(
            "INSERT INTO audit_logs (id, action, target_type, target_id, metadata, ip_address, created_at)
             VALUES ($1, 'user.registered', 'user', $2, '{}', '198.51.100.7', $3)",
        )
        .bind(id)
        .bind(target)
        .bind(now - Duration::days(200))
        .execute(&app.pool)
        .await
        .unwrap();
    }
    insert(recent, "user.login_failed", Some("who@example.com"), 10).await;
    insert(
        old_failed,
        "user.login_failed",
        Some("who@example.com"),
        200,
    )
    .await;
    insert(old_other, "user.password_changed", Some("ana"), 200).await;
    insert(ancient, "user.password_changed", Some("ana"), 6 * 365).await;

    apply_audit_retention(&app.pool).await.unwrap();

    let row = |id: Uuid| {
        let pool = app.pool.clone();
        async move {
            sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT ip_address, target_label FROM audit_logs WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(
        row(recent).await,
        Some((Some("203.0.113.9".into()), Some("who@example.com".into())))
    );
    assert_eq!(row(old_failed).await, Some((None, None)));
    assert_eq!(row(old_other).await, Some((None, Some("ana".into()))));
    assert_eq!(row(ancient).await, None);
    assert_eq!(
        row(live_signup).await.unwrap().0.as_deref(),
        Some("198.51.100.7")
    );
    assert_eq!(row(gone_signup).await.unwrap().0, None);
}

/// Every column pointing at `users.id` either feeds the personal data
/// export (`EXPORTED_USER_REFERENCES`) or is listed here with the reason it
/// doesn't. Driven by the live schema: a new table holding personal data
/// fails this test until someone decides where it goes.
#[tokio::test]
async fn every_reference_to_an_account_is_exported_or_excluded_on_purpose() {
    use setlyst_api::services::data_export::EXPORTED_USER_REFERENCES;

    const EXCLUDED: &[&str] = &[
        // Repertoire content: exported by `/backup/export` (and
        // `/songs/export/chordpro`), in a format that can be re-imported.
        "artists.user_id",
        "songs.user_id",
        "setlists.user_id",
        "gigs.user_id",
        "tours.user_id",
        // "Who last changed this" on records that are not the account's
        // own data (band content, other people's accounts, platform
        // configuration). The account's own actions are in `security_log`.
        "announcements.created_by",
        "announcements.updated_by",
        "artists.updated_by",
        "artists.deleted_by",
        "band_notes.updated_by",
        "band_song_suggestions.resolved_by",
        "bands.created_by",
        "bands.updated_by",
        "gigs.updated_by",
        "gigs.deleted_by",
        "gigs.share_locked_by",
        "moderation_flags.resolved_by",
        "plans.updated_by",
        "platform_settings.updated_by",
        "promo_codes.created_by",
        "promotions.created_by",
        "release_notes.created_by",
        "release_notes.updated_by",
        "setlists.updated_by",
        "setlists.deleted_by",
        "setlists.share_locked_by",
        "songs.updated_by",
        "songs.deleted_by",
        "subscriptions.updated_by",
        "tours.updated_by",
        "tours.deleted_by",
        "user_quotas.updated_by",
        "users.banned_by",
        "users.created_by",
        "users.updated_by",
        // Staff acting on the account (impersonation, credit adjustments,
        // plan changes): the outcome is exported, the staff member isn't.
        "audit_logs.impersonator_id",
        "credit_ledger.actor_id",
        "subscription_events.actor_id",
        // Invite codes are the band's credentials; the band's admins see
        // who created each one.
        "band_invites.created_by",
        // Covered through `referrals` (`referrals_made` / `referred`).
        "users.referred_by",
        // Second-factor material: secrets, never exported (the account's
        // 2FA state is in `account`).
        "totp_recovery_codes.user_id",
        "second_factor_attempts.user_id",
        // Short-lived abuse counters (wiped within a day) and lockout
        // bookkeeping; the sign-in attempts themselves are in `security_log`
        // and the codes requested (with their address) in `one_time_codes`.
        "login_failures.user_id",
        "reauth_attempts.user_id",
        "verification_code_requests.user_id",
        // Provider references for Checkout pages, expired within a day;
        // the resulting subscription and payments are exported.
        "checkout_sessions.user_id",
    ];

    let app = match TestApp::spawn().await {
        Some(app) => app,
        None => return,
    };
    let references: Vec<String> = sqlx::query_scalar(
        "SELECT c.conrelid::regclass::text || '.' || a.attname
         FROM pg_constraint c
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = ANY (c.conkey)
         WHERE c.contype = 'f' AND c.confrelid = 'users'::regclass
         ORDER BY 1",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();

    let unaccounted: Vec<&String> = references
        .iter()
        .filter(|r| {
            !EXPORTED_USER_REFERENCES.contains(&r.as_str()) && !EXCLUDED.contains(&r.as_str())
        })
        .collect();
    assert!(
        unaccounted.is_empty(),
        "columns referencing users that the personal data export neither covers nor excludes: {unaccounted:?} \
         (add them to services/data_export.rs, or to EXCLUDED here with the reason)"
    );
    // Both lists stay honest about the schema.
    for listed in EXPORTED_USER_REFERENCES.iter().chain(EXCLUDED) {
        assert!(
            references.iter().any(|r| r == listed),
            "{listed} no longer references users"
        );
    }
}

#[tokio::test]
async fn the_personal_data_export_covers_band_activity_and_communication() {
    let app = match TestApp::spawn().await {
        Some(app) => app,
        None => return,
    };
    let (user_id, token) = app
        .user("exporter", setlyst_api::models::user::Role::User)
        .await;
    let band = app.band(&token, "Export Band").await;
    let note = app
        .post(
            &format!("/bands/{band}/notes"),
            &token,
            serde_json::json!({ "content": "Bring the capo" }),
        )
        .await;
    assert_eq!(note.status, StatusCode::CREATED, "{}", note.body);
    app.post(
        &format!("/bands/{band}/favorite"),
        &token,
        serde_json::json!({}),
    )
    .await;

    let export = setlyst_api::services::data_export::personal_data(&app.state, user_id)
        .await
        .unwrap();
    assert_eq!(
        export["band_notes_authored"][0]["content"],
        "Bring the capo"
    );
    assert_eq!(export["favorites"]["bands"][0]["band"], "Export Band");
    assert!(
        export["preferences"].is_null() || export["preferences"].get("communication").is_some()
    );
    assert!(export["moderation"]["about_me"].as_array().is_some());
    assert!(export["emails_sent"].as_array().is_some());
    assert!(export["suggestion_votes"].as_array().is_some());
}

#[tokio::test]
async fn consents_are_recorded_in_the_legal_ledger() {
    let app = app!();
    let response = app
        .request_with_headers(
            axum::http::Method::POST,
            "/auth/register",
            None,
            Some(serde_json::json!({
                "username": "consenting", "email": "consenting@example.com",
                "password": common::STRONG_PASSWORD, "accept_terms": true,
                "age_confirmed": true, "marketing_opt_in": false
            })),
            &[("user-agent", "LedgerTest/1.0")],
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
    let user_id: Uuid = response.body["id"].as_str().unwrap().parse().unwrap();
    let rows: Vec<(String, bool, String, Option<String>)> = sqlx::query_as(
        "SELECT document, accepted, source, user_agent FROM legal_acceptances
         WHERE user_id = $1 ORDER BY document",
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    let documents: Vec<(&str, bool)> = rows.iter().map(|r| (r.0.as_str(), r.1)).collect();
    assert_eq!(
        documents,
        vec![
            ("age_declaration", true),
            ("marketing_email", false),
            ("privacy_policy", true),
            ("terms_of_use", true),
        ]
    );
    assert!(rows.iter().all(|r| r.2 == "register"));
    assert_eq!(rows[0].3.as_deref(), Some("LedgerTest/1.0"));

    // Turning marketing on in the settings adds a row; unrelated changes
    // don't.
    let token = app.login("consenting", common::STRONG_PASSWORD).await;
    app.put(
        "/users/me/communication",
        &token,
        serde_json::json!({ "categories": { "marketing": { "email": true, "in_app": true } } }),
    )
    .await;
    app.put(
        "/users/me/communication",
        &token,
        serde_json::json!({ "categories": { "bands": { "email": false, "in_app": true } } }),
    )
    .await;
    let marketing: Vec<(bool, String)> = sqlx::query_as(
        "SELECT accepted, source FROM legal_acceptances
         WHERE user_id = $1 AND document = 'marketing_email' ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        marketing,
        vec![
            (false, "register".to_string()),
            (true, "settings".to_string())
        ]
    );

    // The ledger outlives the account.
    let deleted = app
        .request(
            axum::http::Method::DELETE,
            "/users/me",
            Some(&token),
            Some(serde_json::json!({ "confirmation": "consenting", "password": common::STRONG_PASSWORD })),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let kept: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM legal_acceptances WHERE user_id IS NULL")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(kept, 5);
}

#[tokio::test]
async fn unsubscribing_from_marketing_is_recorded_in_the_legal_ledger() {
    use setlyst_api::{email::unsubscribe, models::communication::Category};
    let app = app!();
    let response = app
        .register(
            "newsletter.reader",
            "newsletter.reader@example.com",
            serde_json::json!({ "marketing_opt_in": true }),
        )
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
    let user_id: Uuid = response.body["id"].as_str().unwrap().parse().unwrap();
    let one_click = |category| {
        format!(
            "/public/email/unsubscribe/one-click?token={}",
            unsubscribe::token(user_id, category)
        )
    };
    let marketing_rows = || async {
        sqlx::query_as::<_, (bool, String, Option<String>)>(
            "SELECT accepted, source, user_agent FROM legal_acceptances
             WHERE user_id = $1 AND document = 'marketing_email' ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&app.pool)
        .await
        .unwrap()
    };
    let audited = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_logs
             WHERE action = 'user.communication_changed' AND target_id = $1
               AND metadata->>'source' = 'unsubscribe_link'",
        )
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
    };

    // The mail provider's one-click POST withdraws the marketing consent.
    for _ in 0..2 {
        let done = app
            .request_with_headers(
                axum::http::Method::POST,
                &one_click(Category::Marketing),
                None,
                None,
                &[("user-agent", "MailProvider/1.0")],
            )
            .await;
        assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    }
    let rows = marketing_rows().await;
    assert_eq!(
        rows.iter().map(|r| (r.0, r.1.as_str())).collect::<Vec<_>>(),
        vec![(true, "register"), (false, "unsubscribe_link")],
        "one withdrawal, however many clicks"
    );
    assert_eq!(rows[1].2.as_deref(), Some("MailProvider/1.0"));
    assert_eq!(audited().await, 1);

    // Other categories are audited but aren't consents.
    let done = app
        .post_public(
            "/public/email/unsubscribe",
            serde_json::json!({ "token": unsubscribe::token(user_id, Category::Announcements) }),
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    assert_eq!(marketing_rows().await.len(), 2);
    assert_eq!(audited().await, 2);
}
