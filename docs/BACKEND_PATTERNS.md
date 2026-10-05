# Backend patterns (cookbook)

How to build a feature crate on the shared foundation. Every snippet uses
real APIs; read the referenced files for details. `docs/ARCHITECTURE.md`
is the binding design; this is the how-to.

Reference implementations: `crates/bgh-repos` (routes, permissions, jobs,
git) and `crates/bgh-accounts` (auth, sessions, validation).

## 1. Crate anatomy

Your crate (e.g. `bgh-issues`) already exists and is mounted. Edit only
inside it:

```rust
// crates/bgh-issues/src/lib.rs
pub fn router() -> Router<AppState> {          // paths relative to /api/v3
    Router::new()
        .route("/repos/{owner}/{repo}/labels", get(labels::list).post(labels::create))
        .route("/repos/{owner}/{repo}/labels/{name}", get(labels::get).delete(labels::delete))
}
pub fn web_router() -> Router<AppState> {      // absolute paths: /_bgh/..., raw downloads
    Router::new()
}
pub fn register(reg: &mut Registry) {          // jobs + event listeners
    reg.job(jobs::reindex_issue);
    reg.on_event("issues.timeline", timeline::on_event);
}
```

Shared code that two crates need goes into `bgh-core`, never a dependency on
another domain crate's internals.

## 2. A handler

```rust
use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;   // AppState, ApiError/ApiResult/FieldError, Json/Path/Query,
                            // MaybeUser/RequireUser/RequireSiteAdmin/AuthContext,
                            // Permission/RepoAccess, Pagination/Page, Tx, SyncAction,
                            // Event, Timestamp, api::*, db::*

pub async fn get_label(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<api::Label>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?; // 404 if no read
    let row: db::Label = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 AND lower(name) = lower($2)", db::Label::COLUMNS))
        .bind(access.repo.id).bind(&name)
        .fetch_optional(&state.db).await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(api::Label::new(&state.urls, &access.owner.login, &access.repo.name, &row)))
}
```

* Use `bgh_core::extract::{Json, Path, Query}` (in the prelude), not the
  axum originals: their rejections render GitHub errors (400 "Problems
  parsing JSON", 422, 404). `Json` also accepts bodies without a JSON
  content type, like GitHub.
* SQL: runtime `sqlx::query_as::<_, T>` with `#[derive(FromRow)]` structs,
  always `.bind(...)`; select columns with the row struct's `COLUMNS` const
  (`db::prefixed("r", db::Repository::COLUMNS)` when joining).
* Return `(StatusCode::CREATED, Json(x))` for 201 and `StatusCode::NO_CONTENT`
  for 204.

## 3. Authentication

| Extractor | Meaning |
|-----------|---------|
| `MaybeUser` | `Option<AuthContext>`; bad credentials still → 401 |
| `RequireUser` | 401 "Requires authentication" if anonymous; derefs to `AuthContext` |
| `RequireSiteAdmin` | site admin (and `site_admin` scope for tokens), else 403 |

`AuthContext { user: db::User, method, scopes }`. Check token scopes with
`auth.require_scope("repo")?` / `auth.has_scope("admin:org")` (sessions have
every scope; the scope hierarchy `repo ⊃ public_repo` etc. is built in).
`auth.is_session()` distinguishes browser sessions (e.g. token management).
Resolution happens once per request (cached in request extensions), and
`X-OAuth-Scopes` is added automatically for token requests.

Git transport and other non-API callers that must accept passwords use
`bgh_core::auth::authenticate(&state, &headers, AuthOptions { allow_password: true })`.

## 4. Permissions

```rust
let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?; // 404 unless Read
access.require(Permission::Triage)?;      // 403 for readers lacking it, 404 for others
access.require_not_archived()?;           // 403 on archived repos (writes)
access.permission                         // effective permission (token scopes applied)
access.api_permission()                   // Some(p) when authenticated → JSON `permissions`
access.scope()                            // "repo:{id}" sync scope
```

Org-level checks: `perms::org_role(&state.db, org_id, user_id)` →
`Some("admin" | "member")`. For lists, never compute permissions per row:
use `perms::repo_permissions(db, user_id, &repos)` (one query) or the
render helper `bgh_core::views::minimal_repos(&state, auth, rows)`, which
also drops repositories the caller can't read. Batch-load users with
`views::users_by_id(&state, ids)`.

Never reveal private resources: no read access ⇒ `ApiError::NotFound`.

## 5. Errors

```rust
ApiError::NotFound
ApiError::requires_auth()                     // 401
ApiError::forbidden("Must have admin rights to Repository.")
ApiError::conflict("...")                     // 409
ApiError::invalid_field(FieldError::missing_field("Label", "name"))   // 422
ApiError::validation(vec![FieldError::invalid("Label", "color"), ...])
ApiError::invalid_field(FieldError::custom("Repository", "name", "name already exists on this account"))
ApiError::unprocessable("Reference does not exist")                   // 422, no errors[]
```

`sqlx::Error::RowNotFound` → 404, other DB/Redis/IO errors → 500 (logged,
never shown). Map unique-index races explicitly:

```rust
.await.map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
    Some("labels_repo_name_key") => ApiError::invalid_field(FieldError::already_exists("Label", "name")),
    _ => e.into(),
})?;
```

## 6. GitHub JSON

* Shared shapes live in `bgh_core::models::api` and are built from DB rows:
  `SimpleUser::new(&state.urls, &user)` (`SimpleUser::or_ghost` for deleted
  authors), `PublicUser`, `PrivateUser`, `OrganizationSimple/Full`,
  `MinimalRepository::new(&urls, &repo, &owner, perm)`, `Repository::new`
  (full), `Label::new`, `Milestone::new`, `ReactionRollup::from_counts`,
  `TeamSimple::new`, `AuthorAssociation`. Add new shared shapes there;
  crate-local shapes (e.g. `Issue`) may live in the crate but must follow
  the same rules.
* Field names and null-vs-missing exactly as GitHub's docs: `Option<T>`
  serializes as `null`; use `#[serde(skip_serializing_if = "Option::is_none")]`
  only for fields GitHub omits.
* Timestamps: `bgh_core::time::Timestamp` (`2024-01-01T00:00:00Z`);
  `ts(opt)` for optional DB times.
* URLs: always `state.urls` (`urls.repo(o, r)`, `urls.issue_html(o, r, n)`,
  `urls.label(o, r, name)`, `urls.api("/path")`, `urls.html("/path")`), never
  hand-built from config. `RepoLinks::new` has every repo `*_url` template.
* `node_id`: `bgh_core::node_id::encode(NodeType::Issue, id)`.
* Markdown bodies (`body_html`, rendered views):
  `bgh_core::markdown::render(text, &RenderContext::new(&state.config.base_url).with_repo(owner, repo))`
  — GFM + sanitization + `@mention`/`#123`/SHA links.

## 7. Pagination

```rust
pub async fn list(State(state): State<AppState>, auth: MaybeUser, p: Pagination,
                  Path((owner, repo)): Path<(String, String)>) -> ApiResult<Page<api::Label>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<db::Label> = sqlx::query_as(&format!(
        "SELECT {} FROM labels WHERE repo_id = $1 ORDER BY lower(name), id LIMIT $2 OFFSET $3",
        db::Label::COLUMNS))
        .bind(access.repo.id).bind(p.limit_plus_one()).bind(p.offset())
        .fetch_all(&state.db).await?;
    Ok(p.page(rows).map(|l| api::Label::new(&state.urls, &owner, &repo, &l)))
}
```

`p.page(rows)` detects the next page from the extra row and builds the
`Link` header (`prev`/`next`/`first`). With a known count use
`p.page_with_total(rows, total)` (adds `last`; fetch with `p.limit()`).
Wrapped bodies (search `{total_count, items}`) can call
`p.link_header(has_next, Some(total))` directly. `per_page` defaults to 30,
max 100. Always add a deterministic tiebreaker (`, id`) to `ORDER BY`.

## 8. Writes: transaction, sync, audit, events, jobs

```rust
let mut tx = Tx::begin(&state).await?;
let label: db::Label = sqlx::query_as(&format!(
    "INSERT INTO labels (repo_id, name, color) VALUES ($1, $2, $3) RETURNING {}", db::Label::COLUMNS))
    .bind(access.repo.id).bind(&name).bind(&color)
    .fetch_one(&mut *tx).await?;                                   // &mut *tx is a PgConnection
bgh_core::audit::log(&mut *tx, Some(&auth.user), "label.create",
    bgh_core::audit::Target::Repo { id: access.repo.id, org_id: None }, json!({"name": name})).await?;
tx.enqueue(&ReindexRepo { repo_id: access.repo.id }).await?;    // runs after commit
tx.emit(Event::RepositoryUpdated { repo_id: access.repo.id, actor_id: auth.user.id });
tx.sync_model(SyncModel::Label, label.id, SyncAction::Insert).await?; // last: see 8a
tx.commit().await?;   // commit → publish sync deltas to Redis → emit events
```

* Every synced model change records a sync action **in the same
  transaction** — through the helpers of section 8a, never hand-built JSON.
* Dropping a `Tx` without `commit()` rolls back and discards all side effects.
* Counters (`open_issues_count`, `comments_count`, ...) are updated by the
  owning code in the same transaction (`UPDATE … SET n = n + 1`).
* Low-level equivalents exist (`sync::record` + `sync::notify`,
  `jobs::enqueue_job`, `state.events.emit`) for code that can't use `Tx`.

## 8a. Sync payloads (local-first clients)

The web client keeps a local copy of every *synced model* (`user`, `org`,
`membership`, `team`, `repo`, `viewerRepo`, `label`, `milestone`, `issue`
— PRs included —, `comment`, `review`, `issueEvent`, `notification`;
docs/SYNC_PROTOCOL.md §3) and learns about changes only from
`sync_actions`. **Any write that changes what one of these rows looks like
must record it**, or clients go stale until their next bootstrap.

The compact shapes are built in exactly one place,
`bgh_core::sync::shapes` (SQL that renders rows straight from the tables).
Bootstrap, partial sync and your deltas all use it, so you never write
sync JSON yourself. `SyncModel` (= `shapes::Model`) is in the prelude.

| You changed | Record (inside your `Tx`, after the writes) |
|-------------|------------------------------------------|
| a row's own columns, or its child rows (issue labels/assignees/reactions, PR reviewers/reviews/checks, team members/repos) | `tx.sync_model(SyncModel::X, id, SyncAction::Update)` (`Insert` for new rows) |
| many rows of one model | `tx.sync_models(SyncModel::X, &ids, action)` (one query) |
| an issue / PR | `tx.sync_issue(issue_id, action, body_changed)` — pass `true` when the body was set/edited (inserts with a body too); the lazy `body` is only sent then |
| deleted a row | `tx.sync_delete(&scope, SyncModel::X, id)` (scope: `sync::repo_scope(repo_id)`, `org_scope`, `user_scope`) — the client also drops an issue's comments/reviews/events |
| a user's login/name/avatar | `tx.sync_user(user_id)` (user scope + every org scope of the user) |
| stars, watches, or someone's access to a repo | `tx.sync_viewer_repo(user_id, repo_id)` (records `D` when they lost read access) |

Rows are loaded **inside your transaction**, so they reflect your
uncommitted writes. Derived values follow the data, so re-sync the rows
that show them:

* issue opened/closed/transferred → also `sync_model(Repo, repo_id)`
  (`openIssues`/`openPulls`) and the milestone (`openIssues`/`closedIssues`);
* comment created/deleted → also the issue (`comments`);
* label deleted (cascades `issue_labels`) → `sync_models(Issue, &affected)`
  before the delete commits, then `sync_delete` the label;
* review submitted/dismissed, reviewer requested, check run/status
  changed → the PR's issue (`reviewDecision`, `checks`);
* org member added/removed → `sync_model(Membership, ..)` /
  `sync_delete(&org_scope, Membership, id)`, the teams whose `memberIds`
  changed, and `sync_viewer_repo` for the member's affected repos.

Rules:

* **Record last.** `record` takes a transaction-scoped advisory lock so
  sync ids commit in id order (clients resume from "everything ≤ N"); it is
  held until commit, so do the slow work first and the `tx.sync*` calls
  right before `tx.commit()`.
* **Access changes:** after removing a collaborator / team grant /
  membership, changing visibility or transferring a repository, also
  `tx.emit(Event::AccessChanged { repo_id, org_id, user_id })` (any ids you
  know). bgh-sync rechecks live WebSockets and revokes lost scopes
  (`repo`/`org`/`membership`/`team`/`viewerRepo` deltas and
  `RepositoryUpdated`/`RepositoryDeleted` events trigger it too, and a
  5-minute sweep is the safety net). Ending sessions through
  `auth::destroy_session` / `destroy_user_sessions` closes that user's
  sockets with 4001 automatically.
* **Private data:** pending reviews are visible only to their author —
  sync a review when it is submitted, not while pending. Notifications go
  to `user:{recipient}` (`sync_model(Notification, id, ..)`; marking a
  thread done → `sync_delete(&user_scope(uid), Notification, id)`).
* **Timeline events** (`issue_events.data`) are mapped to the client
  shape by key: `label: {id, name, color}`, `assignee_id`,
  `requested_reviewer_id`, `milestone: {title}`, `rename: {from, to}`,
  `state_reason`, and the `commit_id` column. Store new events in that form.
* `X-Client-Tx` is captured automatically: every action written while
  handling a request carries the header's tx (request-scoped context set
  by `bgh_sync::http_middleware`), the response gets `X-Bgh-Sync-Id`, and
  mutations are idempotent per `(user, tx)` for 24 h. Nothing to do in
  handlers.
* Raw `tx.sync(scope, model, id, action, &data)` still works for models
  outside the list above; don't use it for listed models.

Tests: assert the recorded action with
`SELECT model, model_id, action::text, data FROM sync_actions` — `data`
must equal the row `GET /_bgh/sync/bootstrap` returns (see
`crates/bgh-sync/tests/bootstrap.rs::tx_helpers_record_bootstrap_shapes`).

## 9. Background jobs

```rust
#[derive(Serialize, Deserialize)]
pub struct ReindexRepo { pub repo_id: i64 }
impl bgh_core::jobs::JobPayload for ReindexRepo {
    const KIND: &'static str = "search.reindex_repo";   // "<crate>.<action>", unique
    // const MAX_ATTEMPTS: i32 = 10;
}
pub async fn reindex_repo(state: AppState, job: ReindexRepo) -> anyhow::Result<()> { ... }
// lib.rs: pub fn register(reg: &mut Registry) { reg.job(jobs::reindex_repo); }
```

Handlers must be idempotent and tolerate deleted rows (return `Ok(())`).
Errors retry with backoff; panics and timeouts (10 min) count as failures.
Example: `bgh_repos::jobs::post_receive`.

## 10. Events

Add variants to `bgh_core::events::Event` (carry ids, not objects; update
`name()`, `repo_id()`, `actor_id()`). Emit with `tx.emit(..)`. Listen:

```rust
reg.on_event("notify.subscriptions", |state, event: Arc<Event>| async move {
    match &*event {
        Event::IssueOpened { issue_id, .. } => enqueue_notifications(&state, *issue_id).await,
        _ => Ok(()),
    }
});
```

Listeners run in-process, in order, one task per listener; for durable
side effects (webhooks, email) enqueue a job from the listener.

## 11. Git

```rust
let store = bgh_repos::store(&state);   // or bgh_git::RepoStore::from_config(&state.config)
let readme = store.read(repo_id, |r| match r.lookup_path("main", "README.md")? {
    bgh_git::PathLookup::Entry(e) => r.blob(&e.sha),        // size-limited (BGH_MAX_BLOB_SIZE)
    _ => Err(bgh_git::GitError::NotFound("README.md".into())),
}).await?;                                                   // GitError → ApiError (404/422/403/500)
```

`GitRepo` offers `branches()`, `tags()`, `find_ref`, `resolve`,
`resolve_commit`, `commit`, `tag`, `tree`, `lookup_path`, `blob`,
`log(rev, path, skip, limit)`, `is_ancestor`, `head_branch`; `r.gix()`
exposes the raw gix repository. Writes: `bgh_git::write::commit_changes`
(add/modify/delete files on a branch with an expected parent),
`update_ref` / `delete_ref` (old-value checked), `set_head`. Content keyed
by object SHA is immutable: cache it and send
`Cache-Control: public, max-age=31536000, immutable`.

## 12. Migrations

* File: `migrations/NNNN_description.sql` in your crate's range (see
  ARCHITECTURE.md; core used 0001-0006). Never edit a merged migration.
* `BIGINT GENERATED ALWAYS AS IDENTITY` ids, `TIMESTAMPTZ NOT NULL DEFAULT
  now()`, FKs with explicit `ON DELETE` (`CASCADE` for owned rows, `SET
  NULL` for authors → rendered as ghost), `CHECK` constraints for enums,
  role names `read|triage|write|maintain|admin`, case-insensitive
  uniqueness via `UNIQUE INDEX … (lower(name))`. Index every FK and every
  list query's `WHERE … ORDER BY` shape.
* Migrations are embedded at compile time (`bgh_core::db::MIGRATOR`); the
  test template DB re-migrates automatically. Core tables already cover
  issues, PRs, reviews, statuses/checks, webhooks, notifications,
  releases, keys, teams — extend them with `ALTER TABLE` in your range
  rather than creating parallel tables.

## 13. Integration tests

```rust
// crates/bgh-issues/tests/labels.rs   (dev-deps already set up)
use serde_json::json;

#[tokio::test]
async fn creates_label() {
    let app = bgh_server::test_app().await;               // fresh DB, full router
    let alice = app.create_user("alice").await;           // + full-scope PAT
    app.create_repo(&alice, "hello").await;               // via the real API
    let res = app.post("/api/v3/repos/alice/hello/labels").auth(&alice)
        .json(&json!({"name": "bug", "color": "f00000"})).send().await;
    res.assert_status(201);
    assert_eq!(res.json()["url"], app.url("/api/v3/repos/alice/hello/labels/bug"));
}
```

Fixtures: `create_user`, `create_admin`, `create_token(&user, &["repo"])`,
`session_cookie(&user)`, `create_org(login, &admin)`, `add_org_member`,
`create_repo`, `create_private_repo`, `create_repo_with(&user, org, body)`.
Requests: `app.get/post/patch/put/delete(path)` then `.auth(&user)`,
`.token(t)`, `.basic(u, p)`, `.cookie(c)`, `.header(k, v)`, `.json(&v)`,
`.body(bytes)`, `.send().await` → `TestResponse` (`status()`,
`assert_status(n)`, `json()`, `json_as::<T>()`, `header(name)`, `text()`).
Also: `app.drain_jobs().await` (run queued jobs now),
`app.state.events.subscribe()` (assert emitted events), `app.state.db`
(direct SQL setup/asserts), `app.url(path)` / `app.git_remote(&user, owner,
repo)` for the real TCP listener. Custom config:
`TestApp::spawn_with_config(bgh_server::factory(), |c| c.signup_enabled = false)`.
Extra test-only jobs/listeners: `TestApp::spawn_with(AppFactory { router:
bgh_server::app, register: my_register })` (see `bgh-server/tests/infra.rs`).

When shelling out (e.g. `git`) inside a test, use `tokio::process` — the
server runs on the test's runtime, so blocking calls deadlock. See
`crates/bgh-repos/tests/git_transport.rs`.

Requires Postgres + Redis (`./scripts/dev-setup.sh`). `cargo test -p <crate>`.

## 14. Before committing

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <your-crate>            # and the workspace before merging
```
