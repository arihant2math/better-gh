# Better GitHub — agent guide

Read `docs/ARCHITECTURE.md` before changing anything, and
`docs/BACKEND_PATTERNS.md` before writing backend code. It is the source of
truth for layout, API compatibility rules, migrations ranges, and the sync
engine. If you must deviate, update the doc in the same commit.

## Setup

```
./scripts/dev-setup.sh          # starts postgres + redis, creates DBs
export DATABASE_URL=postgres://postgres:postgres@localhost/bgh
export REDIS_URL=redis://127.0.0.1/
cargo run -p bgh-server         # http://localhost:3000
cd web && npm install && npm run dev
./scripts/dev.sh                # or: both together (cargo-watch if installed)
./scripts/gh-compat.sh          # real `gh` CLI against a throwaway server
./scripts/api-smoke.sh          # curl + jq checks of core REST shapes
```

Deployment (Docker, systemd, proxies): `docs/SELF_HOSTING.md`.

## Conventions

* Rust 2024 edition, stable toolchain. `cargo fmt`, `cargo clippy
  --workspace --all-targets -- -D warnings` must pass.
* SQL: runtime-checked `sqlx::query_as::<_, T>(...)` with `#[derive(FromRow)]`
  (no compile-time `query!` macros — no DB needed to build). Always bind
  parameters; never format SQL with user input.
* Errors: return `bgh_core::error::ApiError` (renders GitHub-style JSON).
  Use `ApiError::not_found()` when the caller lacks read access to a
  private resource (don't leak existence).
* Every API resource JSON must match GitHub's REST shape; serialize via
  structs in `bgh_core::models::api` (shared shapes like SimpleUser,
  Repository, Label live there).
* Synced model writes must call `bgh_core::sync::record` in the same
  transaction (see ARCHITECTURE.md "Sync engine").
* Keep domain crates independent; shared code goes to `bgh-core`.
* Tests: integration tests per crate under `tests/` using
  `bgh_server::test_app().await` (a `bgh_core::testing::TestApp` with a
  fresh database). Run `cargo test -p <crate>`.
* Domain crates expose `router()` (paths relative to `/api/v3`),
  `web_router()` (absolute paths) and `register(&mut Registry)`; they are
  already mounted in bgh-server, so feature work stays inside one crate.
* Writes go through `bgh_core::db::Tx` (`tx.sync`, `tx.emit`,
  `tx.enqueue`, then `tx.commit()`).
* Web: TypeScript strict, `npm run lint && npm run typecheck && npm run
  build` in `web/` must pass. Respect the bundle budget in `web/README.md`.
* Commit messages: imperative, scoped (`issues: add label endpoints`).
