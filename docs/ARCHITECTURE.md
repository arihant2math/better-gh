# Better GitHub — Architecture

A self-hostable, GitHub-compatible forge for teams. Rust monolith backend
(Postgres + Redis), local-first React web client. Goal: GitHub's feature set
and API surface, Linear's speed.

## Guiding performance principles (Linear-style)

1. **Never wait on the network for UI.** The web client keeps a local
   normalized database (in-memory store mirrored to IndexedDB). Reads come from
   it; mutations are applied optimistically and sent to the server in the
   background. Server deltas arrive over a WebSocket.
2. **Partial bootstrap + delta sync.** On first load the client fetches a
   compact bootstrap for the scopes it needs (orgs/repos it can see), then only
   deltas (`sync_actions` rows newer than `lastSyncId`).
3. **Immutable content is cached forever.** Anything addressed by a git object
   id (blob, tree, rendered/highlighted file, diff between two SHAs) gets
   `Cache-Control: public, max-age=31536000, immutable` and a Redis cache.
4. **Small, modern bundle.** ES2022+ only, route-level code splitting, no
   legacy polyfills, hashed assets served pre-compressed (brotli) with
   immutable caching, app shell cached by a service worker, self-hosted
   variable font preloaded, boot data inlined in `index.html`.
5. **Prefetch on intent.** Hover / keyboard focus on a link prefetches its
   data. Lists are virtualized. Keyboard-first (command palette, shortcuts).
6. **Backend is lean.** Set-based SQL with proper indexes, no N+1 (batch
   loaders), Redis for hot caches/rate limits/pubsub, pooled connections,
   streaming git transport, background jobs for slow work.

## Repository layout

```
Cargo.toml                 workspace
crates/
  bgh-core/                config, AppState, db+redis pools, errors, auth
                           extractors, permissions, pagination, JSON models
                           (GitHub shapes), url builder, markdown, sync log,
                           job queue, event bus, test harness
  bgh-git/                 on-disk repo storage, read ops (gix), write ops
                           (git CLI plumbing), smart-HTTP + SSH transport,
                           diff/blame/highlight, merge
  bgh-accounts/            users, auth/login/sessions, 2FA, PATs, OAuth apps,
                           SSH/GPG keys, emails, orgs, teams, memberships
  bgh-repos/               repos CRUD, collaborators, forks, stars, watching,
                           topics, contents/trees/blobs/commits/refs API,
                           branches, tags, compare, branch protection, deploy
                           keys, git transport routes
  bgh-issues/              issues, labels, milestones, comments, reactions,
                           timeline/events, assignees, locking, templates
  bgh-pulls/               pull requests, reviews, review comments, merge,
                           statuses, check runs/suites, CODEOWNERS, auto-merge
  bgh-notify/              notifications, subscriptions, webhooks, email
  bgh-releases/            releases, assets
  bgh-search/              issues/PR/code/repo/user search
  bgh-admin/               site administration, audit log
  bgh-sync/                local-first sync engine (bootstrap, WS deltas)
  bgh-graphql/             GitHub GraphQL v4 subset (for `gh` CLI, etc.)
  bgh-actions/             CI: workflow parsing, runner orchestration
  bgh-server/              binary `bgh`: composes routers, serves web/dist
migrations/                sqlx migrations (single ordered dir)
web/                       React + TypeScript client (Vite)
docs/                      design docs
scripts/                   dev setup, test helpers
```

Each domain crate exposes exactly three functions, all already wired into
`bgh-server` (so feature work only edits inside its own crate):

* `pub fn router() -> Router<AppState>`: REST routes with paths **relative
  to `/api/v3`** (`.route("/repos/{owner}/{repo}/labels", ...)`); bgh-server
  nests them under `/api/v3` (JSON 404 fallback, ETag/304, CORS,
  `X-GitHub-Media-Type`).
* `pub fn web_router() -> Router<AppState>`: routes with **absolute** paths
  (`/_bgh/...`, git transport, raw/archive downloads), merged at the root.
* `pub fn register(reg: &mut bgh_core::Registry)`: background job handlers
  and event listeners.

Domain crates depend on `bgh-core` (and `bgh-git` when needed),
**never on each other's internals** — shared logic that two domains need
moves into `bgh-core` (or a small `pub` service fn re-exported by the owning
crate, depended on explicitly; e.g. `bgh_accounts::create_user`).
`bgh-git` depends on `bgh-core` only for config and error conversion; it
knows nothing about users or permissions.

The cookbook for feature work is `docs/BACKEND_PATTERNS.md`.

## Configuration

Environment variables, read by `bgh_core::Config::from_env` (defaults in
parentheses): `DATABASE_URL` (`postgres://postgres:postgres@localhost/bgh`),
`REDIS_URL` (`redis://127.0.0.1/`), `BGH_LISTEN` (`0.0.0.0:3000`),
`BGH_BASE_URL` (`http://localhost:3000`, used for every generated URL),
`BGH_DATA_DIR` (`./data`), `BGH_WEB_DIR` (`web/dist`), `BGH_SSH_PORT`
(`2222`), `BGH_SSH_ENABLED`, `BGH_SIGNUP_ENABLED` (`true`),
`BGH_SESSION_TTL_DAYS` (`30`), `BGH_JOB_WORKERS` (`4`),
`BGH_DB_MAX_CONNECTIONS` (`20`), `BGH_REDIS_PREFIX` (`bgh:`, prepended to
every Redis key/channel via `AppState::redis_key`), `BGH_GIT_BIN` (`git`),
`BGH_MAX_BLOB_SIZE` (10 MiB), `BGH_SITE_NAME`.

Runtime site settings (edited by site admins, `site_settings` table) are
read through `bgh_core::settings::load(&state)` (typed `SiteSettings`,
cached 5 s per process): sign-up policy (`open|invite|closed` + allowed
email domains), default repository visibility, max repository size and
per-owner `storage_quotas` (checked on push), organization creation
policy, announcement banner, API rate limits, auth providers (password
login, OIDC), SMTP, maintenance mode. `BGH_SIGNUP_ENABLED=false` still
disables sign-up regardless of the setting.

The `bgh` binary: `bgh [serve]` (migrate + HTTP + job workers + event
listeners, graceful shutdown on SIGINT/SIGTERM), `bgh migrate`,
`bgh admin create-user --login --email --password [--site-admin]`,
`bgh admin create-org --login --admin <user> [--name]`,
`bgh admin create-token --user <login> [--scopes a,b] [--name]
[--expires-in-days]` (prints a PAT), `bgh healthcheck` (probes `/healthz`
on `BGH_LISTEN`; container health checks). Deployment (Docker, systemd,
reverse proxies, backups): `docs/SELF_HOSTING.md`.

## HTTP surface

| Prefix           | Purpose                                              |
|------------------|------------------------------------------------------|
| `/api/v3/...`    | GitHub REST v3 compatible API (GHES style)           |
| `/api/graphql`   | GitHub GraphQL compatible subset                     |
| `/_bgh/...`      | Private web-client endpoints (bootstrap, sync WS, rendered views, login) |
| `/{owner}/{repo}.git/...` and `/{owner}/{repo}/info/refs` etc. | git smart HTTP |
| `/{owner}/{repo}/raw/...`, `/{owner}/{repo}/archive/...` | raw files, archives |
| everything else  | SPA `index.html` (client-side routing)               |

SSH: built-in SSH server (russh) on a configurable port for git only.

### GitHub API compatibility rules

* JSON shapes follow GitHub's REST docs exactly (field names, nesting, null
  vs missing). Every resource includes `id`, `node_id`, `url`, `html_url` and
  the `*_url` templates GitHub returns. Build URLs with `bgh_core::urls`.
* Timestamps: ISO-8601 UTC with `Z`, second precision (`2024-01-01T00:00:00Z`).
* Pagination: `page` / `per_page` (default 30, max 100) with RFC 5988 `Link`
  header (`rel="next"|"prev"|"first"|"last"`). Use `bgh_core::pagination`.
* Errors: `{"message": "...", "documentation_url": "...", "errors": [...]}`
  with GitHub's status codes (404 for no-access to private resources, 422
  validation with `errors: [{resource, field, code}]`, 401, 403, 409).
* Auth: `Authorization: token <t>` / `Bearer <t>` / Basic (user:token) ;
  session cookie for the web client. Scopes via `X-OAuth-Scopes`.
* Headers: `X-GitHub-Media-Type`, `X-RateLimit-*`, `ETag` +
  `If-None-Match` → 304 on GETs.
* Media types: `application/vnd.github+json`, `.raw`, `.html`, `.diff`,
  `.patch` where GitHub supports them.
* `node_id`: GitHub's legacy format, base64 of `"{len:02}:{Type}{id}"`
  (`MDQ6VXNlcjE=` = `04:User1`), encoded/decoded by `bgh_core::node_id`
  (`NodeType` enum; add variants as needed).
* Static files: `web/dist/assets/*` → `Cache-Control: public,
  max-age=31536000, immutable`; other files and the SPA fallback
  (`index.html` for unknown non-API GET paths) → `no-cache`; `.br`/`.gz`
  siblings are served when accepted. Unknown `/api/*` and `/_bgh/*` paths
  get GitHub JSON 404s.

Cross-cutting middleware: maintenance mode (`settings::maintenance_middleware`,
503 + `Retry-After` for API/`_bgh`/git requests except site admins,
`/healthz`, `/_bgh/site`, `/_bgh/session`) and API rate limiting
(`ratelimit::rate_limit_middleware` on `/api/v3`, Redis hourly window per
user / client IP, disabled by default like GHES). Repositories with
`disabled = true` answer 403 "Repository access blocked" to everyone but
site admins (`RepoAccess`). Suspended users get 403 on every credential
(token, session, password).

Compatibility is tested with the official `gh` CLI (`GH_HOST`, GHES mode)
and octokit-style raw requests.

## Data model (Postgres)

* Integer `BIGINT GENERATED ALWAYS AS IDENTITY` primary keys (GitHub ids are
  integers). Timestamps `TIMESTAMPTZ`.
* `users` holds both users and organizations (`type` = 'User' |
  'Organization'), like GitHub, so `owner` lookups are a single index hit
  on `lower(login)`.
* `repositories(id, owner_id, name, ...)` unique on `(owner_id, lower(name))`.
  On disk at `{data_dir}/repos/{id % 256 hex}/{id}.git` (renames are free).
* Issues and PRs share `issues` (`number` sequence per repo via
  `repositories.next_issue_number` incremented `UPDATE ... RETURNING`).
  PR-specific columns live in `pull_requests(issue_id PK, ...)`.
* `issue_events` / timeline items, `comments` (issue comments),
  `review_comments`, `reviews`, `reactions(subject_type, subject_id)`.
* Permission is computed from: repo owner, collaborators, org membership +
  base permission (`org_settings.default_repository_permission`), team
  grants (inherited from parent teams), site admin (→ Admin), public
  visibility (→ Read). `bgh_core::perms::repo_permission(db, user_id,
  repo)` / batched `repo_permissions(db, user_id, &repos)` →
  `Permission { None, Read, Triage, Write, Maintain, Admin }`. Token scopes
  then cap it (`perms::effective`: private repos need `repo`; writes to
  public repos need `repo` or `public_repo`). Handlers use
  `perms::RepoAccess::load(&state, auth, owner, name)` which returns 404
  without read access, then `access.require(Permission::Write)` (403).
* Role names are stored as `read|triage|write|maintain|admin` everywhere
  (`Permission::parse` also accepts GitHub's legacy `pull`/`push`).
* Users and orgs are created through `db::NewUser::insert` /
  `db::insert_org` (core) wrapped by validated services in bgh-accounts.
  The first user account becomes site admin.
* Sessions: random cookie `bgh_session` (HttpOnly, SameSite=Lax, Secure on
  https), stored as SHA-256 in `sessions`, cached in Redis for 5 min.
  PATs: `bghp_` + 40 alphanumerics, stored as SHA-256 with scopes/expiry.
  Basic auth with a password is accepted for git transport only.

### Migrations

`migrations/NNNN_description.sql`. Ranges by area to avoid collisions
between parallel work:

| range     | area      | range     | area       |
|-----------|-----------|-----------|------------|
| 0001-0099 | core      | 0700-0799 | search     |
| 0100-0199 | accounts  | 0800-0899 | admin      |
| 0200-0299 | repos     | 0900-0999 | sync       |
| 0300-0399 | issues    | 1000-1099 | actions    |
| 0400-0499 | pulls     | 1100-1199 | projects   |
| 0500-0599 | notify    | 1200-1299 | wiki/misc  |
| 0600-0699 | releases  | 1300+     | later      |

Never edit a migration that has been merged to the integration branch;
add a new one.

## Sync engine (local-first)

The wire protocol (bootstrap, WebSocket messages, partial sync, model shapes,
optimistic-mutation reconciliation) is specified normatively in
[`docs/SYNC_PROTOCOL.md`](SYNC_PROTOCOL.md); this section is a summary.

* Use `bgh_core::db::Tx` (a transaction that collects post-commit side
  effects): `tx.sync(scope, model, id, action, &data)` records the row in
  the transaction and publishes it after `tx.commit()`; `tx.emit(event)` and
  `tx.enqueue(&job)` likewise. Published messages are the JSON of
  `bgh_core::sync::SyncRecord` (`{"id","scope","model","mid","a","d"}`).
* Every mutation of a synced model appends to `sync_actions(id BIGSERIAL,
  scope TEXT, model TEXT, model_id BIGINT, action CHAR(1) /*I,U,D*/, data
  JSONB, tx UUID NULL, created_at)` **in the same transaction** via
  `bgh_core::sync::record(&mut tx, scope, model, id, action, data)`. `tx` is
  the request's `X-Client-Tx` header (taken from the request context), so
  the delta echoes it. `record` serializes writers with a transaction-scoped
  advisory lock so sync ids become visible in id order. After commit, call
  `bgh_core::sync::notify(&state, ...)` which publishes on the Redis channel
  `sync:{scope}`.
* Scopes: `repo:{id}` (repo, issues + PR metadata, labels, milestones,
  comments, reviews, timeline events), `user:{id}` (notifications,
  viewer-specific repo data: permission/starred), `org:{id}` (org,
  memberships, teams). `data` is the compact client shape (camelCase), not
  the GitHub REST shape.
* `GET /_bgh/sync/bootstrap?scopes=...` → `{schemaVersion, lastSyncId,
  userId, scopes, denied, models: {issue: [...], label: [...], ...}}`.
  Without `scopes` the server picks the viewer's default scope set.
* `GET /_bgh/sync/partial?model=comment,review,issueEvent&issue=ID` loads lazy
  models (comments, reviews, timeline events, issue bodies) on demand.
* `GET /_bgh/sync/ws` WebSocket. Client → `{"t":"sub","scopes":[...],
  "since":N}`; server replays missed actions then streams live
  `{"t":"delta","id":N,"scope":..,"model":..,"mid":..,"a":"U","d":{...},
  "tx":..}` (or `{"t":"batch","items":[...]}`), then `{"t":"ready"}`.
  Server rechecks permissions on subscribe; emits `{"t":"revoke","scope"}`;
  `{"t":"rebootstrap"}` when `since` is older than the retained log.
* Mutations go through the normal REST API with an `X-Client-Tx` header so
  the client can reconcile its optimistic write with the echoed delta. The
  server answers with `X-Bgh-Sync-Id` and treats `X-Client-Tx` as an
  idempotency key (24 h).
* Large/cold data (file contents, diffs, highlighted blobs) is fetched on
  demand and cached (immutable when keyed by SHA).

## Audit log

`audit_log` rows are written with `bgh_core::audit::log` /
`log_with_ip` in the transaction of the action (actions use GitHub's
dotted names: `repo.create`, `user.login`, `org.rename`, ...). Searched by
site admins at `/_bgh/admin/audit-log` and in GitHub's shape at
`/orgs/{org}/audit-log` / `/enterprises/{e}/audit-log` (id cursors).
Site-level account changes also emit `UserAccountChanged` /
`OrganizationChanged` events, the source of global webhooks.

## Background work

* Job queue in Postgres (`jobs` table, `FOR UPDATE SKIP LOCKED`), worker
  tasks spawned by `bgh-server` (`BGH_JOB_WORKERS`), woken by `NOTIFY
  bgh_jobs` on commit with a 5 s poll fallback. Typed payloads implement
  `bgh_core::jobs::JobPayload` (`KIND = "<crate>.<action>"`); enqueue with
  `tx.enqueue(&job)` / `jobs::enqueue_job(db, &job)`; register with
  `reg.job(handler)`. Failures retry with exponential backoff (5 s … 1 h)
  until `MAX_ATTEMPTS`, then keep the row with `failed_at`. Handlers must be
  idempotent. Tests run jobs deterministically with `app.drain_jobs()`. Used for: webhook delivery,
  email, post-receive processing (PR sync, mergeability), search indexing,
  repo deletion, archive generation, CI dispatch.
* Event bus: domain events (`bgh_core::events::Event`, a `#[non_exhaustive]`
  enum carrying ids) emitted after commit via `tx.emit`; consumers register
  `reg.on_event(name, handler)` and receive every event in order (one task
  per listener). Delivery is in-process and best-effort: listeners that
  must not lose work enqueue a job. Consumers: webhooks, notifications,
  timeline, search indexing.

## Git

* Storage: `bgh_git::RepoStore` (`{data_dir}/repos/{id % 256:02x}/{id}.git`,
  bare, created with an empty template and server config: no auto-gc,
  `uploadpack.allowFilter`, ...). Forks are `clone --bare --shared`
  (alternates) — a source repo with forks must be repacked into them
  before deletion (TODO). Repo deletion removes the row immediately and
  the directory in the `repos.delete_storage` job.
* Reads via `gix` (fast, in-process): refs, trees, blobs, commits, log.
  gix is used for the object database and refs only; commit/tree/tag
  bytes are parsed by `bgh_git::objects` (stable across gix releases).
  gix is blocking: async code calls `store.read(repo_id, |r| ...)`, which
  runs on the blocking pool. Path-filtered log shells out to `git log`.
* Writes / complex ops via the `git` CLI (merge: `git merge-tree
  --write-tree`, commit-tree, update-ref with old-value checks).
* Smart HTTP: spawn `git upload-pack/receive-pack --stateless-rpc`, stream
  bodies (gzip aware, `Git-Protocol` → `GIT_PROTOCOL`, so protocol v2 works
  for fetch). Protocol machinery lives in `bgh_git::smart_http`; the routes
  (`/{owner}/{repo}[.git]/info/refs|git-upload-pack|git-receive-pack`),
  auth and permission checks live in `bgh-repos`. Ref updates are parsed
  from the receive-pack command list before forwarding and passed to an
  authorize callback (branch protection: locked branches, deletions,
  required PRs, push restrictions — force-push detection needs the objects
  and is TODO). After git exits, refs are re-read to determine which
  updates applied; bgh-repos then enqueues `repos.post_receive` (pushed_at,
  size, default branch on first push, sync record, `Event::Push`) before
  responding. All git subprocesses run with an isolated config
  (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL=/dev/null`).
* LFS batch API + object storage on disk.
* Highlighted/rendered output cached in Redis keyed by blob SHA.

## Web client (web/)

* React 19 + TypeScript + Vite; MobX for the normalized reactive store;
  IndexedDB persistence; small router; CSS modules with design tokens
  (light/dark), no CSS-in-JS runtime.
* Keyboard-first: command palette (⌘K), `g i`, `c`, `j/k` navigation etc.
* Virtualized lists and diffs; skeleton-free instant navigation from local
  data; prefetch on hover.
* Built assets served by `bgh-server` with brotli precompression, from
  `BGH_WEB_DIR` by default or compiled into the binary with the cargo
  feature `embed-web` (release/Docker builds; a `BGH_WEB_DIR` containing
  an `index.html` still wins).

## Testing

* `bgh_server::test_app().await` (feature `testing`, enabled in every
  crate's dev-dependencies) returns a `bgh_core::testing::TestApp`: the full
  router on a fresh Postgres database (`CREATE DATABASE … TEMPLATE
  bgh_test_tpl_<hash of migrations>`; the template is migrated once per
  process under an advisory lock, and keying it by the migration set lets
  branches with different migrations test concurrently), a unique Redis key
  prefix, a temp data dir, and a real `127.0.0.1` port for git CLI tests.
  Databases are dropped when the `TestApp` drops (`BGH_TEST_KEEP_DB=1` keeps
  them); leftovers of dead processes are cleaned up on the next run.
* Each domain crate has integration tests in `tests/` hitting the HTTP
  router with real requests and asserting GitHub-compatible JSON.
* `scripts/gh-compat.sh` exercises the real `gh` CLI (GHES mode, behind a
  throwaway TLS proxy) against a fresh server or a running one and reports
  PASS/FAIL/SKIP per command (`--json` for machine-readable results);
  `scripts/api-smoke.sh` checks core REST shapes with curl + jq. Both start
  `bgh` on a temporary database by default (`scripts/lib/test-server.sh`).
