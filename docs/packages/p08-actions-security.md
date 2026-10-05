# P8 — Actions token and runner security: status

Branch `bgh/p08-actions-security`. Migration `2000_actions_token_permissions.sql`
(range 2000–2099).

**Status:** complete (scope and acceptance of PHASE4_PLAN.md §P8); see
"Deviations" and "Known gaps".

## Runner isolation

* Steps (and docker CLI calls) never inherit the server environment:
  `runner::process::run_process` always `env_clear()`s, then adds the
  allowlist `runner::process::HOST_ENV` (PATH, HOME, USER, LOGNAME, SHELL,
  LANG/LANGUAGE/LC_ALL/LC_CTYPE, TZ, TERM, TMPDIR, DOCKER_* CLI variables,
  proxy variables, SSL_CERT_FILE/DIR) and the job env. `DATABASE_URL`,
  `REDIS_URL`, `BGH_*`, SMTP settings are never visible.
* `RunnerConfig::detect_executor` returns `Option`: `auto` without a working
  docker is `None`, never shell. The built-in runner then logs a loud warning
  and takes no jobs (they stay queued for external runners). `shell` must be
  set explicitly and logs a start-up warning (trusted single-tenant only).
  The external `bgh-runner` binary still falls back to shell on its own host
  (with a warning), like GitHub's self-hosted runner.
* Work dir: `BGH_ACTIONS_WORK_DIR`, default `{tmp}/bgh-actions-work`
  (`services::builtin_work_dir`); a value inside `BGH_DATA_DIR` is rejected
  with an error log and replaced by the default.
* `Dockerfile` / `docker-compose.yml` set `BGH_ACTIONS_EXECUTOR=auto` with an
  explanation (the image has no docker, so the built-in runner is idle);
  `docs/SELF_HOSTING.md` has a new "Actions (CI)" section and config rows.

## GITHUB_TOKEN permissions

* `bgh_core::token_permissions` (new): `Category` (GitHub's 16 categories,
  `pull-requests` and `pull_requests` spellings), `Access` (none/read/write),
  `TokenPermissions` (map; metadata always read; `read_all`, `write_all`,
  `none`, `restricted_default` = contents+packages read,
  `permissive_default` = write except id-token, `read_only`), and the
  route-category table `classify(method, path) -> Need` (REST paths under
  `/repos/{o}/{r}/…` by area, administration/secrets/runners/hooks/user
  account endpoints forbidden, unknown reads = metadata, unknown writes
  forbidden). `graphql_mutation_need(field)` maps GraphQL mutations.
* `token_permissions::middleware` is mounted by bgh-server for every route
  (inside the sync request scope): for job tokens, `/api/v3` calls must be
  covered by the map (403 "Resource not accessible by integration"),
  `/_bgh` writes are refused. Reads of *other* repositories pass through
  (`perms::effective` already caps them to anonymous access).
* Git transport (bgh-repos `git_http::git_access`): job tokens need
  `contents: write` to push and `contents: read` to fetch a private repo.
* GraphQL: `bgh_graphql::mutation::guard` checks the mutation name.
* `perms::effective`: a job token gets Write on its repository (Read when its
  map has no write) regardless of the token user's own role (the user is the
  bot now).
* Workflows: `StoredJob.permissions` (job-level, else workflow-level);
  `server::prepare_spec` resolves it (site default otherwise; fork
  `pull_request` runs → read-only, no secrets), stores the map in
  `access_tokens.permissions`, mirrors it into the token's scopes
  (`actions:permission:<category>:<access>`, so enforcement needs no query),
  and sends it to the runner (`JobSpec.token_permissions`), which prints a
  "GITHUB_TOKEN Permissions" group in "Set up job" like GitHub.
* Site setting `actions` section (`SiteSettings.actions`):
  `default_workflow_permissions` (`read` default | `write`, validated) and
  `can_approve_pull_request_reviews` (stored; not enforced yet — P30). Admin
  UI: new "Actions" section on `/admin/settings` (RadioCards).

## `workflow` scope

* `bgh_repos::workflow_scope`: PAT / OAuth tokens without `workflow` may not
  create, update or delete `.github/workflows/**`; job tokens never may;
  sessions, passwords and SSH keys may. Messages are GitHub's
  (`refusing to allow a Personal Access Token|an OAuth App to create or update
  workflow `<path>` without `workflow` scope`, job tokens: `… a GitHub App …
  without `workflows` permission`).
* Push: `PushPolicy.workflow_denied` (bgh-git) → the `pre-receive` hook
  checks the pushed commits (`git log --name-only old..new` or `new --not
  --all`) while quarantined, so a rejected push leaves nothing behind.
* Contents API: `PUT`/`DELETE /repos/{o}/{r}/contents/{path}` → 403.

## Loop guard and attribution

* `bgh_core::bots`: `github-actions[bot]` (type `Bot`, GitHub's id
  41898282), created on first token mint (`ensure_actions_bot`).
* Job tokens belong to the bot, so comments, labels, releases, pushes and
  contents writes are authored by it.
* `Event::via_actions_token()` (actor is the bot); `trigger::on_event`
  ignores such events except dispatches.
* Audit: the triggering actor travels as `actions:actor:<id>` scope (also in
  `access_tokens.created_by_id`); auth notes it in the request context
  (`sync::context::note_actions_actor`), and `audit::log*` adds
  `triggering_actor` / `triggering_actor_id` to entries written by the bot.

## Shared-code changes (additive)

* bgh-core: new `token_permissions.rs`, `bots.rs`; `perms::{JOB_TOKEN_ACTOR_SCOPE_PREFIX, job_token_actor}` and the job-token branch of
  `effective` (cap instead of `raw.min(cap)`); `auth::authenticate` notes
  the triggering actor; `sync::context::{note_actions_actor, actions_actor}`
  (new `RequestSync` field); `audit::log_with_ip` triggering actor;
  `Event::via_actions_token`; `settings::ActionsSettings` + `actions` section.
* bgh-git: `PushPolicy.workflow_denied`, `WORKFLOW_PATH_PLACEHOLDER`, hook
  block.
* bgh-server: mounts `token_permissions::middleware`.
* bgh-graphql: mutation guard check.
* bgh-repos: `workflow_scope.rs`, checks in `contents.rs` / `git_http.rs`.
* bgh-actions: `JobSpec.token_permissions` (serde default),
  `StoredJob.permissions` (serde default), `server::token_permissions`.

## Tests

* `crates/bgh-actions/tests/it/security.rs`: contents:read token can't push
  (HTTP) or write contents, can fetch; issues:write comments/labels as
  `github-actions[bot]`; administration/secrets/user endpoints 403; GraphQL
  mutation refused; default token read-only and site default `write`;
  write token pushes / contents / release as the bot without re-triggering
  (a human push still triggers), audit `triggering_actor`; job tokens can't
  touch workflow files (API + push); `write-all`; fork PR token read-only
  with no secrets; PAT `workflow` scope over HTTP push (create, delete, new
  branch with old commits OK, password OK) and contents API (PUT/DELETE,
  session OK); shell-executor job's `env` shows no CARGO_/DATABASE_URL/
  REDIS_URL/BGH_/SMTP/RUST_ variables.
* Unit: `token_permissions` (defaults, scopes, route table), process env
  allowlist, `detect_executor` (auto → None), `workflow_scope` paths.
* Web: `src/pages/admin/settingsForm.test.ts`.
* `scripts/actions-e2e.sh`: the lint job runs `env | sort`; the script fails
  if `DATABASE_URL|REDIS_URL|BGH_|SMTP` appear in its log.

## Deviations

* No new field on `PushEvent`/other events: the loop guard keys off the
  actor (every job-token write is attributed to the bot, which nobody else
  can act as), exposed as `Event::via_actions_token()`. This avoids touching
  every event construction site across crates.
* SSH pushes use keys (full user credentials, like GitHub), so the
  `workflow` scope check has nothing to restrict over SSH; deploy keys are
  allowed too (GitHub allows them). The hook mechanism is shared, so a
  future token-over-SSH path would only need to set `workflow_denied`.
* The permission map is enforced from the token's scopes (mirror of the
  JSON column) to avoid a query per request.

## Known gaps / TODO

* Repo/org overrides of the default permissions and
  `can_approve_pull_request_reviews` enforcement: P30.
* API ref writes (`/git/refs`, merges) are not checked for the `workflow`
  scope (GitHub checks them too); LFS uploads by job tokens are bounded by
  the coarse Write cap, not `contents: write`.
* The route table covers the endpoints this server implements; new areas
  (packages registry, deployments, pages) are pre-mapped by path.
