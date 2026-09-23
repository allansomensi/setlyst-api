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
- **Account security** — Sign-in by username or e-mail, per-account lockout after repeated failures, two-factor authentication (TOTP, RFC 6238) with single-use recovery codes, e-mail verification and change by code, password recovery by e-mail, Google sign-in, self-service account deletion and consent tracking
- **Transactional e-mail** — Localized (`en`, `pt-BR`, `es`) templates delivered through an outbox and a background worker (SMTP via `lettre`), with per-category communication preferences and one-click unsubscribe links
- **Announcements & release notes** — Staff-published announcements (modal, banner, notification, e-mail) targeted by role, plan and language, and editable "What's new" notes
- **Plans & billing** — Plans with feature flags and limits (enforced only when switched on), trials, promo codes, promotions, credits, referral rewards and complimentary grants; card subscriptions through Stripe Checkout, with in-place plan changes and the Stripe billing portal
- **Moderation** — Automatic checks of usernames, avatars and band logos (word list, blocked domains, optional image classification), user reports and a staff queue
- **Rate Limiting** — Per-client-IP rate limiting (trusted-proxy aware) on sign-in, password recovery, Google sign-in and globally across all endpoints
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

Edit `.env` and set a secure `JWT_SECRET` (minimum 32 characters), a `DATA_ENCRYPTION_KEY` (generate it with `openssl rand -base64 32`; the server won't start without one) and your database credentials.

PDF export reads its fonts from `assets/fonts`. When the binary runs from another directory (a deployment, a service manager), point `ASSETS_DIR` at a copy of `assets`; missing fonts are logged as an error at startup.

**3. Start the database**

```bash
just services-up
```

**4. Run database migrations**

Migrations run automatically when the server starts (set `RUN_MIGRATIONS=false` to opt out). To run them by hand:

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
| `IMPERSONATION_EXPIRATION_TIME` | Lifetime of a staff "view as" session, in seconds | `3600` |
| `DATABASE_URL` | Full PostgreSQL connection URL | — |
| `DATABASE_MAX_CONNECTIONS` | Connection pool size | `10` |
| `RUN_MIGRATIONS` | Apply pending migrations on startup | `true` |
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
| `APP_BASE_URL` | Public web origin used in e-mail links | `http://localhost:3000` |
| `DATA_ENCRYPTION_KEY` | **Required.** 32-byte key (base64) for AES-256-GCM encryption of secrets at rest (TOTP seeds). Generate with `openssl rand -base64 32` and never change it (stored 2FA secrets become unreadable). The server refuses to start without it | — |
| `ALLOW_DERIVED_DATA_KEY` | Local development only: `true` derives the data key from `JWT_SECRET` when `DATA_ENCRYPTION_KEY` is unset (a warning is logged; rotating `JWT_SECRET` then breaks every 2FA account) | `false` |
| `ASSETS_DIR` | Directory containing `fonts/` (Inter TTFs used by PDF export). Missing fonts are reported at startup | `./assets`, else the source tree's `assets` |
| `PDF_FONTS_DIR` | Overrides the font directory itself (legacy; prefer `ASSETS_DIR`) | `<ASSETS_DIR>/fonts` |
| `TRUSTED_PROXIES` | Comma-separated CIDRs whose `X-Forwarded-For` is trusted. Empty = forwarding headers are ignored | empty |
| `INTERNAL_API_SECRET` | Shared secret the web server sends as `X-Setlyst-Internal`, with the visitor's address in `X-Setlyst-Client-IP` | — |
| `GOOGLE_CLIENT_IDS` | Comma-separated OAuth client IDs accepted for Google sign-in. Empty = disabled | empty |
| `SMTP_HOST` | SMTP server. When unset, e-mails are rendered and logged only | — |
| `SMTP_PORT` | SMTP port. Hosts that block the standard ports (Render's free instances block 25, 465 and 587) need an alternative such as Resend's `2465` with `SMTP_TLS=tls` | `587` / `465` / `25` by `SMTP_TLS` |
| `SMTP_USERNAME` / `SMTP_PASSWORD` | SMTP credentials | — |
| `SMTP_TLS` | `starttls`, `tls` or `none` | `starttls` |
| `SMTP_FROM` | Sender, e.g. `Setlyst <no-reply@setlyst.app>` | `Setlyst <no-reply@setlyst.app>` |
| `SMTP_REPLY_TO` | Optional reply-to address | — |
| `EMAIL_WORKER_INTERVAL_SECS` | E-mail outbox polling interval | `10` |
| `STRIPE_SECRET_KEY` | Stripe secret (`sk_...`) or restricted (`rk_...`) key. With `STRIPE_WEBHOOK_SECRET`, turns paid checkout on | — |
| `STRIPE_WEBHOOK_SECRET` | Signing secret (`whsec_...`) of the webhook endpoint `/api/v1/webhooks/stripe` | — |
| `STRIPE_API_BASE` | Stripe API origin (only for stripe-mock or a proxy) | `https://api.stripe.com` |
| `MODERATION_VISION_API_KEY` | Google Cloud Vision key for image classification. Unset = URL heuristics only | — |
| `TRASH_RETENTION_DAYS` | Days before trashed items are purged | `30` |
| `ENABLE_SWAGGER` | Serve `/swagger-ui` and `/api-docs/openapi.json` | `true` |
| `TEST_DATABASE_URL` | PostgreSQL server used by the integration tests (tests are skipped when unset) | — |

---

## API Reference

Full interactive documentation is available via Swagger UI at `/swagger-ui` when the server is running.

### Endpoint Groups

| Tag | Base Path | Description |
|---|---|---|
| Auth | `/api/v1/auth` | Login (password, 2FA, Google), register, password recovery, token verification |
| Users | `/api/v1/users` | User management, profiles, preferences |
| Artists | `/api/v1/artists` | Artist CRUD |
| Songs | `/api/v1/songs` | Song CRUD, ChordPro export |
| Bands | `/api/v1/bands` | Band, membership and invite endpoints |
| Setlists | `/api/v1/setlists` | Setlist management, song ordering, PDF export |
| Metrics | `/api/v1/metrics` | User and admin dashboard metrics |
| Backup | `/api/v1/backup` | Data export and import |
| Status | `/api/v1/status` | Health and version (details for staff at `/status/details`) |
| Public | `/api/v1/public` | Legal version, plans, release notes, e-mail unsubscribe |
| Billing | `/api/v1/billing` | The caller's plan, credits, promo codes, referrals, checkout, plan changes and the billing portal |
| Webhooks | `/api/v1/webhooks` | Stripe events (signature-verified, no auth) |
| Announcements | `/api/v1/announcements` | Announcements for the caller |
| Admin | `/api/v1/admin` | Staff console (content, audit log, limits, announcements, release notes, plans, promo codes, moderation) |
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
GET /api/v1/setlists/{id}/export/pdf?show_key=true&show_bpm=true&compact=true&columns=2&font_scale=85&lang=pt-BR
```

Every option is optional. Content: `show_title`, `subtitle`, `show_description`, `show_band_name`, `show_date`, `show_total_duration`, `show_numbers`, `show_artist`, `show_key`, `show_bpm`, `show_song_duration`, `show_tags`, `show_blocks`, `show_breaks`. Songbook: `include_lyrics`, `chords` (`above` | `inline` | `hide`), `page_break_per_song`. Layout: `compact`, `columns` (1–2), `font_scale` (60–200 %), `uppercase_titles`, `watermark`, `page_numbers`, `paper` (`a4` | `letter` | `legal`), `orientation` (`portrait` | `landscape`), `margins` (`narrow` | `normal` | `wide`).

Supported locales: `en`, `pt-BR`, `es`. The file name is sent RFC 5987-encoded, so non-ASCII titles survive.

---

## Staff, Limits & Audit

- **Roles are hierarchical.** Moderators manage regular users only; admins manage users and moderators; nobody manages their own account through the staff endpoints. Only admins change roles, limits and other people's content.
- **Suspensions** (`POST /users/{id}/ban`, with optional duration and reason) and **deactivation** sign the account out immediately — every request re-checks the account state. Prefer them to deletion.
- **View as** (`POST /users/{id}/impersonate`) issues a short-lived, read-only token; every write is refused with `IMPERSONATION_READ_ONLY`.
- **Limits** — platform defaults (`/admin/settings/quotas`) and per-user overrides (`/users/{id}/quotas`) cap songs, setlists, bands, tags and more. Exceeding one answers `QUOTA_EXCEEDED` with the resource and limit.
- **Public links** can be taken down by staff (`/admin/{setlists,gigs}/{id}/share/revoke`); the owner is notified and can't re-share until unlocked.
- **Audit log** (`/admin/audit-logs`) records who did what, when and from where. Records also carry `updated_by` for "last modified by".
- **Errors** carry a stable machine-readable `code` (see `src/errors/api_error.rs`) for clients to translate.

---

## Testing

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test
```

Integration tests (`tests/api.rs`) exercise the HTTP API end to end, each on its own throwaway database.

The Stripe client itself is checked against [stripe-mock](https://github.com/stripe/stripe-mock), which validates requests against Stripe's API spec (skipped unless `STRIPE_MOCK_URL` is set):

```bash
stripe-mock -http-port 12111 &
STRIPE_MOCK_URL=http://localhost:12111 cargo test --test stripe_mock
```

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