//! Integration-test harness.
//!
//! Every test gets its own freshly migrated PostgreSQL database, so tests
//! are isolated and can run in parallel. The server is driven in-process
//! through `tower::ServiceExt::oneshot` — no network listener involved.
//!
//! The database server is taken from `TEST_DATABASE_URL` (falling back to
//! `DATABASE_URL` from the environment or `.env`). When no server is
//! reachable the tests are skipped with a notice instead of failing, so
//! `cargo test` stays usable on machines without Postgres.
//!
//! Each database is dropped again when its [`TestApp`] goes out of scope
//! (best effort: a failure to drop never fails the test).

#![allow(dead_code)]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use setlyst_api::{
    config::Config,
    database::{AppState, connection::run_migrations},
    models::user::{CreateUserPayload, Role, Status},
    routes::api_router,
};
use sqlx::{Connection, PgConnection, PgPool, postgres::PgPoolOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use tower::ServiceExt;
use uuid::Uuid;

pub const STRONG_PASSWORD: &str = "Str0ng!Passw0rd";

fn server_url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
}

/// Replaces the database name in a `postgres://` URL.
fn with_database(url: &str, database: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let base = match base.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{database}"),
        None => format!("{base}/{database}"),
    };
    match query {
        Some(query) => format!("{base}?{query}"),
        None => base,
    }
}

fn init_config(database_url: &str) {
    Config::init_with(Config {
        host: "127.0.0.1:0".to_string(),
        database_url: database_url.to_string(),
        postgres_db: "postgres".to_string(),
        database_max_connections: 5,
        run_migrations: true,
        jwt_secret: "integration-tests-secret-that-is-long-enough".to_string(),
        jwt_expiration_time: 3600,
        impersonation_expiration_time: 600,
        cors_allowed_origins: Vec::new(),
        app_base_url: "https://setlyst.test".to_string(),
        ..Config::default()
    });
}

pub struct TestApp {
    pub router: Router,
    pub pool: PgPool,
    pub state: AppState,
    /// Name of this test's throwaway database, dropped with the app.
    database: String,
    /// Maintenance database URL used to drop it.
    admin_url: String,
}

impl Drop for TestApp {
    /// Drops the test database. `Drop` can't be async and the test's own
    /// runtime may be a single thread busy running this very drop, so the
    /// work happens on a separate thread with its own small runtime.
    /// `WITH (FORCE)` ends the pool's connections (and any background
    /// task still holding one). Best effort: errors are only printed.
    fn drop(&mut self) {
        let database = std::mem::take(&mut self.database);
        let admin_url = std::mem::take(&mut self.admin_url);
        let dropper = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let result = async {
                    let mut admin = PgConnection::connect(&admin_url).await?;
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DROP DATABASE IF EXISTS {database} WITH (FORCE)"
                    )))
                    .execute(&mut admin)
                    .await?;
                    admin.close().await
                }
                .await;
                if let Err(e) = result {
                    eprintln!("⚠️  Could not drop test database {database}: {e}");
                }
            });
        });
        let _ = dropper.join();
    }
}

/// Distinct client IPs so the per-IP rate limiter on `/auth` never trips
/// across the many sign-ins a test suite performs.
static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn next_ip() -> String {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    format!("10.{}.{}.{}", (n >> 16) & 255, (n >> 8) & 255, n & 255)
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Value,
    pub bytes: Vec<u8>,
}

impl TestResponse {
    pub fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }
}

impl TestApp {
    /// Returns `None` (and prints why) when no Postgres server is reachable.
    pub async fn spawn() -> Option<Self> {
        let Some(server) = server_url() else {
            eprintln!("⚠️  Skipping integration test: set TEST_DATABASE_URL to run it.");
            return None;
        };

        let admin_url = with_database(&server, "postgres");
        let mut admin = match PgConnection::connect(&admin_url).await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("⚠️  Skipping integration test: cannot reach Postgres ({e}).");
                return None;
            }
        };

        let database = format!("setlyst_test_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&mut admin)
            .await
            .expect("create test database");

        let url = with_database(&server, &database);
        init_config(&url);

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("connect to test database");
        run_migrations(&pool).await.expect("run migrations");

        // External services are faked: no test ever reaches Google or an
        // image classifier.
        let state = AppState::with_services(
            pool.clone(),
            std::sync::Arc::new(setlyst_api::services::google::FakeGoogleVerifier::new()),
            std::sync::Arc::new(setlyst_api::moderation::OfflineModerationService),
        );
        let router = api_router(state.clone());
        Some(Self {
            router,
            pool,
            state,
            database,
            admin_url,
        })
    }

    pub async fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> TestResponse {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/v1{path}"))
            .header("x-forwarded-for", next_ip());

        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }

        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };

        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

        TestResponse {
            status,
            headers,
            body,
            bytes,
        }
    }

    pub async fn get(&self, path: &str, token: &str) -> TestResponse {
        self.request(Method::GET, path, Some(token), None).await
    }

    pub async fn post(&self, path: &str, token: &str, body: Value) -> TestResponse {
        self.request(Method::POST, path, Some(token), Some(body))
            .await
    }

    pub async fn patch(&self, path: &str, token: &str, body: Value) -> TestResponse {
        self.request(Method::PATCH, path, Some(token), Some(body))
            .await
    }

    pub async fn put(&self, path: &str, token: &str, body: Value) -> TestResponse {
        self.request(Method::PUT, path, Some(token), Some(body))
            .await
    }

    pub async fn delete(&self, path: &str, token: &str) -> TestResponse {
        self.request(Method::DELETE, path, Some(token), None).await
    }

    /// Creates an account straight through the repository — bypassing the
    /// HTTP validation on purpose, so tests can also seed legacy states
    /// (e.g. a password below the current policy).
    pub async fn create_user(&self, username: &str, password: &str, role: Role) -> Uuid {
        let payload = CreateUserPayload {
            username: username.to_string(),
            email: None,
            password: password.to_string(),
            first_name: None,
            last_name: None,
            role: Some(role),
            status: Some(Status::Active),
            require_password_change: Some(false),
        };
        self.state
            .user_repo
            .create(&payload, None, false)
            .await
            .expect("seed user")
            .id
    }

    pub async fn login_response(&self, username: &str, password: &str) -> TestResponse {
        self.request(
            Method::POST,
            "/auth/login",
            None,
            Some(json!({ "username": username, "password": password })),
        )
        .await
    }

    pub async fn login(&self, username: &str, password: &str) -> String {
        let response = self.login_response(username, password).await;
        assert_eq!(response.status, StatusCode::OK, "login: {}", response.body);
        response.body["token"].as_str().unwrap().to_string()
    }

    /// Seeds an account with a strong password and signs it in.
    pub async fn user(&self, username: &str, role: Role) -> (Uuid, String) {
        let id = self.create_user(username, STRONG_PASSWORD, role).await;
        let token = self.login(username, STRONG_PASSWORD).await;
        (id, token)
    }

    pub async fn artist(&self, token: &str, name: &str) -> String {
        let response = self.post("/artists", token, json!({ "name": name })).await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body["id"].as_str().unwrap().to_string()
    }

    pub async fn song(&self, token: &str, artist_id: &str, title: &str) -> TestResponse {
        self.post(
            "/songs",
            token,
            json!({ "title": title, "artist_id": artist_id }),
        )
        .await
    }

    pub async fn setlist(&self, token: &str, title: &str, band_id: Option<&str>) -> String {
        let response = self
            .post(
                "/setlists",
                token,
                json!({ "title": title, "band_id": band_id }),
            )
            .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body["id"].as_str().unwrap().to_string()
    }
}

// ---------------------------------------------------------------------
// v0.12 helpers (accounts, e-mail outbox, billing).
// ---------------------------------------------------------------------

impl TestApp {
    /// Like [`TestApp::request`], with extra headers (sent after the
    /// default ones, so e.g. an extra `x-forwarded-for` value becomes the
    /// resolved client IP).
    pub async fn request_with_headers(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/v1{path}"))
            .header("x-forwarded-for", next_ip());
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        TestResponse {
            status,
            headers,
            body,
            bytes,
        }
    }

    /// Sends JSON requests `(method, path, body)` all at once, each on its
    /// own task, for race-condition tests. Responses in request order.
    pub async fn concurrent(
        &self,
        token: Option<&str>,
        requests: Vec<(Method, String, Value)>,
    ) -> Vec<TestResponse> {
        let mut handles = Vec::with_capacity(requests.len());
        for (method, path, body) in requests {
            let router = self.router.clone();
            let mut builder = Request::builder()
                .method(method)
                .uri(format!("/api/v1{path}"))
                .header("x-forwarded-for", next_ip())
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(token) = token {
                builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            let request = builder.body(Body::from(body.to_string())).unwrap();
            handles.push(tokio::spawn(async move {
                let response = router.oneshot(request).await.unwrap();
                let status = response.status();
                let headers = response.headers().clone();
                let bytes = to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .to_vec();
                let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                TestResponse {
                    status,
                    headers,
                    body,
                    bytes,
                }
            }));
        }
        let mut responses = Vec::with_capacity(handles.len());
        for handle in handles {
            responses.push(handle.await.expect("request task"));
        }
        responses
    }

    /// An unauthenticated JSON POST.
    pub async fn post_public(&self, path: &str, body: Value) -> TestResponse {
        self.request(Method::POST, path, None, Some(body)).await
    }

    /// Self-registration with the required e-mail and consent.
    pub async fn register(&self, username: &str, email: &str, extra: Value) -> TestResponse {
        let mut body = json!({
            "username": username,
            "email": email,
            "password": STRONG_PASSWORD,
            "accept_terms": true,
        });
        if let (Some(obj), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            obj.extend(extra.clone());
        }
        self.post_public("/auth/register", body).await
    }

    /// Registers and signs in; returns the id and a token.
    pub async fn registered_user(&self, username: &str, email: &str) -> (Uuid, String) {
        let response = self.register(username, email, json!({})).await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        let id = response.body["id"].as_str().unwrap().parse().unwrap();
        (id, self.login(username, STRONG_PASSWORD).await)
    }

    /// The newest outbox row for `template`, optionally to `to`:
    /// `(to_email, payload, user_id)`.
    pub async fn last_email(
        &self,
        template: &str,
        to: Option<&str>,
    ) -> Option<(String, Value, Option<Uuid>)> {
        sqlx::query_as(
            "SELECT to_email, payload, user_id FROM email_outbox
             WHERE template = $1 AND ($2::text IS NULL OR LOWER(to_email) = LOWER($2))
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(template)
        .bind(to)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }

    /// The code in the newest e-mail of `template` sent to `to`.
    pub async fn last_code(&self, template: &str, to: &str) -> String {
        let (_, payload, _) = self
            .last_email(template, Some(to))
            .await
            .unwrap_or_else(|| panic!("no {template} e-mail to {to}"));
        payload["code"].as_str().unwrap().to_string()
    }

    /// Lets a new code be requested right away (skips the resend wait).
    pub async fn age_codes(&self, user_id: Uuid) {
        sqlx::query(
            "UPDATE verification_codes SET created_at = created_at - INTERVAL '2 minutes' WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Verifies the e-mail of the signed-in user with the code already sent.
    pub async fn verify_email(&self, token: &str, email: &str) -> TestResponse {
        let code = self.last_code("email_verification_code", email).await;
        self.post("/users/me/email/verify", token, json!({ "code": code }))
            .await
    }

    /// Stores billing settings directly (merged over the defaults).
    pub async fn set_billing(&self, settings: Value) {
        let mut value =
            serde_json::to_value(setlyst_api::models::billing::BillingSettings::default()).unwrap();
        if let (Some(obj), Some(extra)) = (value.as_object_mut(), settings.as_object()) {
            obj.extend(extra.clone());
        }
        sqlx::query(
            "INSERT INTO platform_settings (key, value, updated_at) VALUES ('billing', $1, NOW())
             ON CONFLICT (key) DO UPDATE SET value = $1",
        )
        .bind(value)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Polls `query` (a `SELECT COUNT(*)`) until it returns at least
    /// `expected`, for effects produced by background tasks.
    pub async fn wait_for_count(&self, query: &str, bind: Uuid, expected: i64) -> i64 {
        let mut count = 0;
        for _ in 0..50 {
            count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query.to_string()))
                .bind(bind)
                .fetch_one(&self.pool)
                .await
                .unwrap();
            if count >= expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        count
    }
}

// ---------------------------------------------------------------------
// v0.12 content helpers (bands, songs, quotas).
// ---------------------------------------------------------------------

impl TestApp {
    /// Creates a band owned by the signed-in user; returns its id.
    pub async fn band(&self, token: &str, name: &str) -> String {
        let response = self.post("/bands", token, json!({ "name": name })).await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body["id"].as_str().unwrap().to_string()
    }

    /// Makes `member_token`'s account join `band_id` through an invite
    /// created by `owner_token` with `role` (`member` by default).
    pub async fn join_band(
        &self,
        owner_token: &str,
        member_token: &str,
        band_id: &str,
        role: Option<&str>,
    ) {
        let invite = self
            .post(
                &format!("/bands/{band_id}/invites"),
                owner_token,
                json!({ "role": role }),
            )
            .await;
        assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
        let code = invite.body["code"].as_str().unwrap().to_string();
        let accepted = self
            .post(&format!("/invites/{code}/accept"), member_token, json!({}))
            .await;
        assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    }

    /// Creates an artist and a song; returns the song id.
    pub async fn song_id(&self, token: &str, artist: &str, title: &str) -> String {
        let artist_id = match self.get("/artists?per_page=100", token).await.body["data"]
            .as_array()
            .and_then(|artists| {
                artists
                    .iter()
                    .find(|a| a["name"] == artist)
                    .and_then(|a| a["id"].as_str().map(str::to_string))
            }) {
            Some(id) => id,
            None => self.artist(token, artist).await,
        };
        let response = self.song(token, &artist_id, title).await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body["id"].as_str().unwrap().to_string()
    }

    /// Adds a song to a setlist (expects success).
    pub async fn add_to_setlist(&self, token: &str, setlist_id: &str, song_id: &str) {
        let response = self
            .post(
                &format!("/setlists/{setlist_id}/songs"),
                token,
                json!({ "song_id": song_id }),
            )
            .await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
    }

    /// Titles of the songs in a setlist's running order.
    pub async fn setlist_titles(&self, token: &str, setlist_id: &str) -> Vec<String> {
        let items = self
            .get(&format!("/setlists/{setlist_id}/items"), token)
            .await;
        assert_eq!(items.status, StatusCode::OK, "{}", items.body);
        items
            .body
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["item_type"] == "song")
            .map(|i| i["song"]["title"].as_str().unwrap().to_string())
            .collect()
    }

    /// Overrides some of a user's quotas.
    pub async fn set_quota(
        &self,
        user_id: Uuid,
        overrides: setlyst_api::models::quota::QuotaOverrides,
    ) {
        self.state
            .quota_repo
            .set_user_settings(user_id, &overrides, false, user_id)
            .await
            .expect("set quota");
    }

    /// Subscriptions enforced, nobody on a plan: only admins have features.
    pub async fn enforce_billing(&self) {
        self.set_billing(json!({ "enforced": true })).await;
    }
}
