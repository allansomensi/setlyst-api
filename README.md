<div align="center">

<h1>🎸 Setlyst API</h1>

<p>A REST API for managing setlists, songs, and artists — built for live musicians.</p>

[![Rust](https://img.shields.io/badge/rust-2024-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![Axum](https://img.shields.io/badge/axum-0.8-blue?style=flat-square)](https://github.com/tokio-rs/axum)
[![PostgreSQL](https://img.shields.io/badge/postgresql-18-336791?style=flat-square&logo=postgresql)](https://www.postgresql.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![Version](https://img.shields.io/badge/version-0.8.0-purple?style=flat-square)](./Cargo.toml)

</div>

---

## Overview

**Setlyst API** is the backend powering [Setlyst](https://github.com/allansomensi/setlyst-web) — a musician assistant designed for live performances. It handles everything from song and artist management to full setlist organization, with support for PDF export, ChordPro format, data backup/restore, and per-user preferences.

Built with Rust for reliability and performance, using Axum, SQLx, and PostgreSQL.

---

## Features

- **User management** — Registration, authentication (JWT), role-based access control (`user`, `moderator`, `admin`), and per-user preferences (theme, language, live mode font size)
- **Artists & Songs** — Full CRUD with pagination, user-scoped data isolation, uniqueness enforcement, and metadata fields (tonality, BPM, genre, duration, lyrics)
- **Setlists** — Create and manage ordered song lists, reorder tracks, and compute total duration automatically
- **PDF Export** — Generate printable setlist PDFs with optional title, duration, key, and BPM display; supports `en`, `pt-BR`, and `es` locales
- **ChordPro Export** — Export all songs as a single `.cho` file compatible with ChordPro readers
- **Backup & Restore** — Export/import a full portable JSON snapshot of all user data; atomic import with smart merge rules
- **Rate Limiting** — IP-based rate limiting on auth routes and globally across all endpoints
- **OpenAPI / Swagger UI** — Interactive API documentation available at `/swagger-ui`
- **Structured Logging** — Console and optional rolling file logs with configurable levels via `RUST_LOG_CONSOLE` / `RUST_LOG_FILE`
- **Graceful Shutdown** — Handles `SIGTERM` and `Ctrl+C` cleanly

---

## Tech Stack

| Layer | Technology |
|---|---|
| Language | Rust |
| Web Framework | [Axum](https://github.com/tokio-rs/axum) |
| Async Runtime | [Tokio](https://tokio.rs/) |
| Database | PostgreSQL 18 |
| ORM / Query Builder | [SQLx](https://github.com/launchbadge/sqlx) |
| Authentication | JWT ([jsonwebtoken](https://github.com/Keats/jsonwebtoken)) |
| Password Hashing | Argon2 |
| Validation | [validator](https://github.com/Keats/validator) |
| API Docs | [utoipa](https://github.com/juhaku/utoipa) + Swagger UI |
| PDF Generation | [genpdf](https://github.com/elegaanz/genpdf) |
| Rate Limiting | [tower_governor](https://github.com/benwis/tower-governor) |
| Containerization | Docker Compose |

---

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Docker](https://www.docker.com/) & Docker Compose
- [`just`](https://github.com/casey/just) (task runner)
- [`cargo-watch`](https://github.com/watchexec/cargo-watch) (optional, for hot reload)

### Setup

**1. Clone the repository**

```bash
git clone https://github.com/allansomensi/setlyst-api.git
cd setlyst-api
```

**2. Configure environment**

```bash
cp .env.example .env
```

Edit `.env` and set a secure `JWT_SECRET` (minimum 32 characters) and your database credentials.

**3. Start the database**

```bash
just services-up
```

**4. Run database migrations**

```bash
just migrate-run
```

**5. Start the server**

```bash
just serve
```

The API will be available at `http://127.0.0.1:8000`.  
Swagger UI: `http://127.0.0.1:8000/swagger-ui`

---

## Environment Variables

| Variable | Description | Default |
|---|---|---|
| `JWT_SECRET` | Secret key for JWT signing (min. 32 chars) | — |
| `JWT_EXPIRATION_TIME` | Token expiration in seconds | `86400` |
| `DATABASE_URL` | Full PostgreSQL connection URL | — |
| `POSTGRES_HOST` | Database host | `localhost` |
| `POSTGRES_PORT` | Database port | `5432` |
| `POSTGRES_USER` | Database user | `postgres` |
| `POSTGRES_PASSWORD` | Database password | `postgres` |
| `POSTGRES_DB` | Database name | `local_db` |
| `HOST` | Server bind address | `127.0.0.1:8000` |
| `CORS_ALLOWED_ORIGINS` | Comma-separated allowed origins | `http://localhost:3000` |
| `RUST_LOG_CONSOLE` | Console log level | `info` |
| `RUST_LOG_FILE` | File log level | `trace` |
| `LOG_TO_FILE` | Enable rolling file logs | `false` |

---

## API Reference

Full interactive documentation is available via Swagger UI at `/swagger-ui` when the server is running.

### Endpoint Groups

| Tag | Base Path | Description |
|---|---|---|
| Auth | `/api/v1/auth` | Login, register, token verification |
| Users | `/api/v1/users` | User management, profiles, preferences |
| Artists | `/api/v1/artists` | Artist CRUD |
| Songs | `/api/v1/songs` | Song CRUD, ChordPro export |
| Setlists | `/api/v1/setlists` | Setlist management, song ordering, PDF export |
| Metrics | `/api/v1/metrics` | User and admin dashboard metrics |
| Backup | `/api/v1/backup` | Data export and import |
| Status | `/api/v1/status` | Database info |
| Health | `/api/v1/health` | API health |
| Migrations | `/api/v1/migrations` | Admin-only migration runner |

### Authentication

All protected endpoints require a `Bearer` token in the `Authorization` header:

```
Authorization: Bearer <your_jwt_token>
```

### Creating a Superuser

```bash
just create-superuser
# or with a specific username:
just create-superuser -- --username admin
```

## Backup Format

The backup system uses a versioned, self-contained JSON structure:

```json
{
  "version": 1,
  "exported_at": "2026-01-01T00:00:00",
  "artists": [...],
  "songs": [...],
  "setlists": [
    {
      "title": "Black Night",
      "songs": [{ "song_id": "...", "position": 1 }]
    }
  ]
}
```

**Import merge rules:**
- Existing artists and songs (matched by name/title) are reused — not duplicated
- Setlists are always created fresh
- The entire import is atomic — any failure rolls back completely

---

## PDF Export

Generate a printable setlist via:

```
GET /api/v1/setlists/{id}/export/pdf?show_title=true&show_key=true&show_bpm=true&show_total_duration=true&lang=en
```

Supported locales: `en`, `pt-BR`, `es`.

---

## Rate Limiting

| Scope | Limit |
|---|---|
| Global (all endpoints) | 60 req/burst, 1 req/200ms per IP |
| Auth (login, register) | 5 req/burst, 1 req/2s per IP |

## Contributing

Contributions are welcome! Please open an issue first to discuss what you'd like to change.

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/my-feature`)
3. Commit your changes
4. Push to your branch and open a Pull Request

Pre-commit and pre-push hooks run `cargo test`, `cargo clippy`, and `cargo fmt` automatically via [cargo-husky](https://github.com/rhysd/cargo-husky).

---

## License

This project is licensed under the [MIT License](./LICENSE).