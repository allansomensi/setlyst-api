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
    });
}

pub struct TestApp {
    pub router: Router,
    pub pool: PgPool,
    pub state: AppState,
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

        let state = AppState::new(pool.clone());
        let router = api_router(state.clone());
        Some(Self {
            router,
            pool,
            state,
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
