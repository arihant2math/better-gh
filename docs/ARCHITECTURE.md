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

Each domain crate exposes `pub fn router() -> axum::Router<AppState>` (and
optionally `pub fn web_router()` for non-API routes) and is merged in
`bgh-server`. Domain crates depend on `bgh-core` (and `bgh-git` when needed),
**never on each other's internals** — shared logic that two domains need
moves into `bgh-core` (or a small `pub` service fn re-exported by the owning
crate, depended on explicitly).

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
* `node_id`: base64 of `"0{type_code}:{id}"`-style opaque id, decoded by the
  GraphQL layer (`bgh_core::node_id`).

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
  base permission, team grants. `bgh_core::perms::repo_permission(user,
  repo) -> Permission { None, Read, Triage, Write, Maintain, Admin }`
  (cached per request).

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

## Background work

* Job queue in Postgres (`jobs` table, `FOR UPDATE SKIP LOCKED`), worker
  tasks spawned by `bgh-server`. `bgh_core::jobs::enqueue(&tx, kind, payload)`.
  Job handlers are registered by domain crates. Used for: webhook delivery,
  email, post-receive processing (PR sync, mergeability), search indexing,
  repo deletion, archive generation, CI dispatch.
* Event bus: domain events (`bgh_core::events::Event`) emitted after commit;
  consumers: webhooks, notifications, timeline, sync.

## Git

* Reads via `gix` (fast, in-process): refs, trees, blobs, commits, log.
* Writes / complex ops via the `git` CLI (merge: `git merge-tree
  --write-tree`, commit-tree, update-ref with old-value checks).
* Smart HTTP: spawn `git upload-pack/receive-pack --stateless-rpc`, stream
  bodies (gzip aware). Ref updates are parsed from the receive-pack command
  list before forwarding to enforce branch protection; post-receive work is
  done in-process after the pack is accepted.
* LFS batch API + object storage on disk.
* Highlighted/rendered output cached in Redis keyed by blob SHA.

## Web client (web/)

* React 19 + TypeScript + Vite; MobX for the normalized reactive store;
  IndexedDB persistence; small router; CSS modules with design tokens
  (light/dark), no CSS-in-JS runtime.
* Keyboard-first: command palette (⌘K), `g i`, `c`, `j/k` navigation etc.
* Virtualized lists and diffs; skeleton-free instant navigation from local
  data; prefetch on hover.
* Built assets embedded/served by `bgh-server` with brotli precompression.

## Testing

* `bgh_core::testing::TestApp` spins up an app against a fresh Postgres
  database (created per test from a template DB) and a Redis db index.
* Each domain crate has integration tests in `tests/` hitting the HTTP
  router with real requests and asserting GitHub-compatible JSON.
* `scripts/gh-compat.sh` exercises the real `gh` CLI against a running
  server.
