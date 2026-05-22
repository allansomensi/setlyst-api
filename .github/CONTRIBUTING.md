# Contributing to Setlyst API

Thank you for taking the time to contribute! This document covers everything you need to get started — from setting up your environment to submitting a pull request.

---

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Workflow](#development-workflow)
- [Project Structure](#project-structure)
- [Coding Standards](#coding-standards)
- [Database Migrations](#database-migrations)
- [Testing](#testing)
- [Submitting a Pull Request](#submitting-a-pull-request)
- [Reporting Issues](#reporting-issues)

---

## Code of Conduct

This project follows a simple rule: **be respectful**. Constructive criticism is welcome; personal attacks are not. Contributors who create a hostile environment will be removed.

---

## Getting Started

### Prerequisites

Make sure you have the following installed:

- [Rust](https://rustup.rs/) (stable toolchain, edition 2024)
- [Docker](https://www.docker.com/) & Docker Compose
- [`just`](https://github.com/casey/just) — task runner
- [`cargo-watch`](https://github.com/watchexec/cargo-watch) — optional, for hot reload during development
- [`sqlx-cli`](https://github.com/launchbadge/sqlx/tree/main/sqlx-cli) — required for creating migrations

```bash
cargo install just cargo-watch sqlx-cli
```

### Fork & Clone

```bash
# Fork the repo on GitHub, then:
git clone https://github.com/<your-username>/setlyst-api.git
cd setlyst-api
```

### Configure the environment

```bash
cp .env.example .env
```

Edit `.env` and set a valid `JWT_SECRET` (minimum 32 characters) along with your local database credentials.

### Start the database and run

```bash
just services-up    # Starts the PostgreSQL container
just migrate-run    # Applies all pending migrations
just serve          # Starts the API with hot reload
```

The API will be available at `http://127.0.0.1:8000`.  
Swagger UI: `http://127.0.0.1:8000/swagger-ui`

---

## Development Workflow

### Branching

Always branch off from `main`. Use the following naming convention:

| Type | Branch name |
|---|---|
| New feature | `feat/short-description` |
| Bug fix | `fix/short-description` |
| Refactor | `refactor/short-description` |
| Documentation | `docs/short-description` |
| Migration | `migration/short-description` |

```bash
git checkout main
git pull origin main
git checkout -b feat/my-feature
```

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>

[optional body]
```

**Types:** `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`

**Examples:**

```
feat(setlist): add song reorder endpoint
fix(auth): return 401 instead of 500 on expired token
docs(readme): update environment variable table
refactor(song): extract uniqueness check into repository trait
```

Keep the subject line under 72 characters. Use the body to explain *why*, not *what*.

---

### Key Conventions

- **Controllers are thin.** Business logic lives in repositories or utility modules. A controller handler should: extract inputs, delegate, log, return a response.
- **Repositories follow the trait + impl pattern.** Every repository exposes a `trait` (used for `Arc<dyn Repo>` in `AppState`) and a concrete `*RepositoryImpl` backed by `PgPool`. This enables future mocking in tests.
- **All mutations use transactions.** Any repository method that performs more than one write must use `self.db.begin()` and commit explicitly.
- **User data is always scoped.** Every query on artists, songs, and setlists must filter by `user_id`. Never return data across user boundaries.
- **Errors are typed.** Return `ApiError` variants — never panic or use `.unwrap()` in request paths.

---

## Coding Standards

This project enforces style automatically via pre-commit and pre-push hooks (powered by [cargo-husky](https://github.com/rhysd/cargo-husky)). They run on every commit and push:

```
cargo fmt --all -- --check
cargo clippy --all --all-targets -- --deny warnings
cargo test
```

You can also run them manually:

```bash
just lint        # Check fmt + clippy
just lint-fix    # Auto-fix fmt + clippy
just test        # Run tests
```

### Style Guidelines

- Run `cargo fmt` before committing. The project uses Unix newlines (`newline_style = "Unix"` in `rustfmt.toml`).
- Zero Clippy warnings allowed. If you need to suppress a lint, add `#[allow(...)]` with a comment explaining why.
- Use `tracing::{debug, info, warn, error}` for all logging. Include structured fields (e.g. `%user_id`, `%error`) instead of interpolating into the message string.
- Prefer `?` over explicit `match` for error propagation.
- Avoid `.unwrap()` and `.expect()` in request-handling code paths. Use them only in startup/initialization code where a panic is the correct failure mode.
- Document public types and functions with `///` doc comments.

---

## Database Migrations

Migrations live in `src/database/migrations/` and are numbered sequentially:

```
0001_add_users_table.sql
0002_add_artists_table.sql
...
```

### Creating a new migration

```bash
just migrate-add <migration_name>
# Example:
just migrate-add add_tags_to_songs
```

This generates a new timestamped file in the migrations folder.

### Rules for migrations

- **Never edit an existing migration file.** If you need to change the schema, create a new migration.
- **Always test both directions.** Make sure `just migrate-run` and `just migrate-down` work cleanly.
- **Prefer additive changes.** Dropping columns or tables that may contain user data must be discussed in an issue first.
- **Use explicit types.** Define PostgreSQL enums in the migration that creates them, not inline in `CREATE TABLE`.

---

## Testing

```bash
just test               # Run all tests
just filter <pattern>   # Run tests matching a pattern
just test-watch         # Re-run tests on file changes
```

When adding a feature or fixing a bug, include a corresponding test. Tests live alongside the code they cover or in a `tests/` module at the crate root.

For repository-level tests, use the `MockClient` pattern or a test database isolated per test run.

---

## Submitting a Pull Request

1. **Open an issue first** for non-trivial changes. This avoids duplicate work and ensures alignment before you invest time coding.

2. **Keep PRs focused.** One concern per PR. Mixing a feature with an unrelated refactor makes review harder.

3. **Fill out the PR template.** Describe what changed, why, and how to test it.

4. **Ensure CI passes.** All hooks must pass locally before you push. PRs with failing checks will not be reviewed until fixed.

5. **Respond to review comments.** Address feedback or explain your reasoning. PRs with no activity for 14 days may be closed.

### PR Checklist

Before opening a pull request, verify:

- [ ] `just lint` passes with no warnings
- [ ] `just test` passes
- [ ] New or changed behavior has test coverage
- [ ] New migrations follow the naming and sequencing conventions
- [ ] Public types and functions have doc comments
- [ ] The PR description explains the motivation and approach

---

## Reporting Issues

Use [GitHub Issues](https://github.com/allansomensi/setlyst-api/issues) to report bugs or request features.

### Bug Reports

Include:
- A clear, descriptive title
- Steps to reproduce
- Expected vs. actual behavior
- Rust version (`rustc --version`), OS, and any relevant env config (redact secrets)

### Feature Requests

Include:
- The problem you're trying to solve
- Your proposed solution or API shape
- Any alternatives you considered
