# B10 actions — status

Crate `bgh-actions` (+ binary `bgh-runner`), migrations `1000_actions.sql`.
Branch `bgh/actions`.

**Status:** complete per WORKPLAN B10 scope (see "Known gaps" below).

## Overview

| Module | What |
|---|---|
| `workflow/` | `.github/workflows/*.yml` model + validating parser (serde_yaml): `on` (push/PR/PR target filters incl. branches/tags/paths + ignores, workflow_dispatch inputs, schedule, release, issues, issue_comment, workflow_call/workflow_run kept raw), env, defaults, concurrency, permissions, jobs (needs, if, strategy.matrix include/exclude/fail-fast/max-parallel, runs-on, container, services, env, timeout-minutes, continue-on-error, outputs, environment), steps. Filter glob language, POSIX cron (`next_after`), matrix expansion (GitHub include/exclude algorithm). 115 unit tests. |
| `expr/` | `${{ }}` expression engine: all contexts by name, operators with GitHub coercion rules, object filters, functions contains/startsWith/endsWith/format/join/toJSON/fromJSON/hashFiles/success/failure/always/cancelled; `interpolate`, `evaluate_template`, `evaluate_condition` (implicit `success() &&`). 92 unit tests. |
| `trigger.rs` | Event listener `actions.trigger` → durable job `actions.trigger`: push (branches/tags/paths via `git diff`), pull_request (workflows from head, ref `refs/pull/N/merge`) / pull_request_target (workflows from base), release published, issues opened/edited/closed/reopened, issue_comment created. `workflow_dispatch` (input validation/typing), `schedule_tick` (cron window per workflow, CAS so multiple processes are safe). Invalid workflow files on push → `startup_failure` run. Default-branch pushes sync `actions_workflows` (names, schedules, deleted). |
| `engine.rs` | Run creation (+ check suite), `run-name`, workflow `concurrency` (one pending run per group, `cancel-in-progress`), scheduler `advance_run` (jobs materialize when needs complete: job `if` with needs/status semantics, matrix expansion, `strategy` context, runs-on labels, timeout, continue-on-error; skipped jobs; fail-fast; max-parallel), cancel / force-cancel, re-run all / failed / single job (new attempt, successful jobs copied with their logs). |
| `server.rs` | Runner protocol server side: job claiming (`FOR UPDATE SKIP LOCKED`, label subset match, repo/org/site runner scopes), `GITHUB_TOKEN` minting, secrets/vars resolution, job env/container/services evaluation at claim time, heartbeats (cancel signal), completion (check run + annotations + summary, token revoked), stale-job reaper, artifact storage/expiry, `LocalBackend` for the built-in runner. |
| `runner/` | Job executor shared by the built-in runner and `bgh-runner` (see below). |
| `web.rs` | `/_bgh/actions/runner/*` HTTP protocol, `/_bgh/actions/download/{token}` signed downloads, `/_bgh/actions/jobs/{id}/logs/stream` SSE live logs. |
| `api/` | GitHub REST endpoints (list below). |
| `services.rs` | Background services: maintenance loop (cron, reaper, artifact expiry) and the built-in runner. |

### Runner

* Built-in runner: service `actions.builtin_runner` (enabled by
  `BGH_ACTIONS_BUILTIN_RUNNER`), registered as a site-wide runner
  `bgh-builtin-<host>` with `BGH_ACTIONS_RUNNER_LABELS`, runs up to
  `BGH_ACTIONS_MAX_JOBS` jobs at once through `server::LocalBackend`.
* External runners: `bgh-runner register --url … --token <registration token>`
  then `bgh-runner run` (HTTP long-poll, `Authorization: RunnerToken`).
* Executors: `docker` (job container = `container:` or
  `BGH_ACTIONS_DEFAULT_IMAGE`, services on a per-job network with aliases,
  job dir bind-mounted at `/__w`) and `shell` (host, explicit only); `auto`
  picks docker when `docker info` works, otherwise the built-in runner
  takes no jobs (`bgh-runner` falls back to shell on its own host). Steps
  start from an empty environment plus `runner::process::HOST_ENV`.
* Steps: `run` (shells bash/sh/python/pwsh/custom), file commands
  (`GITHUB_OUTPUT/ENV/PATH/STEP_SUMMARY/STATE`, heredocs), workflow commands
  (`set-output`, `add-mask`, `add-path`, `group`, `debug/notice/warning/error`
  → annotations, `stop-commands`, `save-state`), secret masking, step/job
  timeouts, cancellation. `uses`: `actions/checkout` (native, from this
  server with the job token), `actions/upload-artifact` /
  `download-artifact` (native), `actions/cache*` (no-op, `cache-hit=false`),
  `docker://`, local `./path` and remote `owner/repo@ref` actions (fetched
  from this server first, then `BGH_ACTIONS_GITHUB_URL`) of type node,
  composite and docker (pre/post for node actions).

### GITHUB_TOKEN

A short-lived `access_tokens` row (`kind = 'app'`) of `github-actions[bot]`
(`bgh_core::bots`, id 41898282) with scopes `repo actions:repo:<id>
actions:actor:<triggering user>` plus the permission map from the job's
`permissions:` (job level, else workflow level, else the site setting
`actions.default_workflow_permissions`, default `read`) mirrored as
`actions:permission:<category>:<access>` scopes and stored in
`access_tokens.permissions`; fork pull requests get it read-only. Enforced
by `bgh_core::token_permissions` (see `docs/packages/p08-actions-security.md`).
Created when a runner claims the job, deleted on completion, expires after
`timeout + 60 min` regardless. Events caused by it never trigger workflows.

## Endpoints

REST (`/api/v3`, GitHub shapes, wrapped lists `{total_count, <key>}` + `Link`):

* Workflows: `GET /repos/{o}/{r}/actions/workflows`, `GET …/workflows/{id|file}`,
  `PUT …/enable`, `PUT …/disable`, `POST …/dispatches` (204, or 200 with
  `return_run_details`), `GET …/timing`, `GET …/workflows/{id}/runs`.
* Runs: `GET /repos/{o}/{r}/actions/runs` (actor, branch, event, status,
  created, head_sha, check_suite_id, exclude_pull_requests), `GET|DELETE
  …/runs/{id}`, `GET …/attempts/{n}`, `GET …/attempts/{n}/jobs`,
  `GET …/attempts/{n}/logs`, `POST …/cancel` (202), `POST …/force-cancel`,
  `POST …/rerun` (201), `POST …/rerun-failed-jobs`, `GET …/jobs?filter=`,
  `GET|DELETE …/logs` (302 → zip), `GET …/artifacts`, `GET …/pending_deployments` (`[]`).
* Jobs: `GET /repos/{o}/{r}/actions/jobs/{id}`, `GET …/logs` (302 → text),
  `POST …/rerun`.
* Artifacts: `GET /repos/{o}/{r}/actions/artifacts?name=`, `GET|DELETE
  …/artifacts/{id}`, `GET …/artifacts/{id}/zip` (302; 410 when expired).
* Secrets: repo (`/actions/secrets`, `public-key`, `{name}` GET/PUT/DELETE,
  `/actions/organization-secrets`), environment
  (`/repos/{o}/{r}/environments/{env}/secrets…`), org
  (`/orgs/{org}/actions/secrets…` incl. visibility and
  `{name}/repositories[/{repo_id}]`).
* Variables: same three levels (`/actions/variables`, `…/organization-variables`,
  `/environments/{env}/variables`, `/orgs/{org}/actions/variables…`).
* Environments (minimal, no protection rules): `GET /repos/{o}/{r}/environments`,
  `GET|PUT|DELETE …/environments/{name}`.
* Runners: repo and org — `GET …/actions/runners`, `GET …/runners/downloads`
  (`[]`), `POST …/registration-token`, `POST …/remove-token`, `GET|DELETE
  …/runners/{id}`, labels `GET|POST|PUT|DELETE …/{id}/labels`, `DELETE
  …/{id}/labels/{name}`.

Web (`/_bgh/actions`): runner protocol (`register`, `self`, `acquire`,
`jobs/{id}/logs|steps|complete|artifacts…`), `download/{token}`,
`jobs/{id}/logs/stream` (SSE).

## Tables (migration 1000)

`actions_workflows`, `actions_runs`, `actions_jobs`, `actions_runners`,
`actions_runner_tokens`, `actions_artifacts`, `actions_environments`,
`actions_keys` (sealed-box key pairs, private key encrypted),
`actions_secrets` (+ `actions_secret_repos`), `actions_variables`
(+ `actions_variable_repos`). Uses core `check_suites`/`check_runs`
(`app_slug = 'actions'`, check run `external_id` = job key,
`details_url` = job page); annotations are stored in `check_runs.output`
(`annotations` array in GitHub annotation shape + `annotations_count`) —
bgh-pulls can serve them from there.

Files: logs `{data_dir}/actions/logs/{job_id}/{step}.log`, artifacts
`{data_dir}/actions/artifacts/{id}.zip`, generated server key
`{data_dir}/actions/server.key`.

Sync models: `workflow_run`, `workflow_job` (scope `repo:{id}`).

## Shared-code changes (all additive)

* `bgh_core::config::ActionsConfig` (`Config.actions`): `BGH_ACTIONS_ENABLED`,
  `BGH_ACTIONS_BUILTIN_RUNNER`, `BGH_ACTIONS_EXECUTOR` (auto|docker|shell),
  `BGH_ACTIONS_DEFAULT_IMAGE`, `BGH_ACTIONS_MAX_JOBS`,
  `BGH_ACTIONS_RUNNER_LABELS`, `BGH_ACTIONS_WORK_DIR`,
  `BGH_ACTIONS_SECRET_KEY`, `BGH_ACTIONS_ARTIFACT_RETENTION_DAYS`,
  `BGH_ACTIONS_REMOTE_ACTIONS`, `BGH_ACTIONS_GITHUB_URL`, `BGH_DOCKER_BIN`.
* `bgh_core::perms::{JOB_TOKEN_SCOPE_PREFIX, JOB_TOKEN_READ_ONLY_SCOPE, job_token_repo}` and the
  job-token branch in `perms::effective`.
* `bgh_core::registry::{Service, Registry::service, spawn_services}`;
  `bgh-server` `main.rs` spawns services (the test harness does not).
* `Event::WorkflowRunUpdated` carries the GitHub REST JSON of the run
  (`workflow_run`) and workflow (`workflow`), rendered by
  `engine::run_event` with the same builder as `GET /actions/runs/{id}`
  (actions: `requested`, `in_progress`, `completed`), so bgh-notify
  delivers `workflow_run` webhooks. `Event::WorkflowJobUpdated` (ids +
  action) is emitted on queued/in_progress/completed; bgh-notify does not
  deliver `workflow_job` webhooks yet. Actions check runs/suites emit
  `CheckRunUpdated` (`created`, `completed`) and `CheckSuiteUpdated`
  (`completed`, actor = triggering user) for `check_run`/`check_suite`
  webhooks and `ci_activity` notifications.
* `NodeType::{Workflow, WorkflowRun, Artifact, Environment}`.
* Workspace deps: serde_yaml, indexmap, zip, crypto_box, chacha20poly1305,
  reqwest. Note: bgh-actions enables serde_json `preserve_order`, which
  unifies workspace-wide (JSON objects keep insertion order).

## Tests

* Unit: expressions, workflow parser/filters/cron/matrix, crypto, logs,
  runner (mock backend; shell + docker executors).
* Integration (`tests/`): `runs.rs` (push/tag/path triggers, PR events,
  dispatch, schedule, matrix/needs/outputs, fail-fast, failure()/always(),
  cancel, concurrency, re-runs, artifacts, logs/zip, job token scope,
  check suites/runs, startup failure), `settings.rs` (secrets at three
  levels incl. sealed boxes and precedence, variables, runners & labels,
  SSE log stream), `e2e.rs` (real workflows executed with the shell
  executor: built-in runner via `services::run_queued_jobs` — checkout from
  this server, outputs across jobs, matrix, artifacts, secret masking,
  annotations, step summary, check suite/runs — and an external runner via
  `HttpBackend` over TCP). Runner unit tests (39) use a mock backend and
  cover the docker executor (skipped when `docker info` fails).

## Known gaps / TODO

* Reusable workflows (`jobs.<id>.uses`) are parsed but fail the job with a
  clear error. `workflow_run` trigger not wired.
* Environments: no protection rules / required reviewers / deployment
  branch policies; no deployments API (`environment.url` ignored).
* Job-level `concurrency` is parsed but not enforced (workflow-level is).
* `actions/cache` is a no-op; no cache API (`/actions/caches`).
* PR runs use the head commit (no `refs/pull/N/merge` merge commit is
  created). Fork pull requests get no secrets and a read-only token
  (`actions:read-only` scope), but there is no "require approval for fork
  PRs" policy yet.
* Log/artifact files of deleted repositories are not swept (rows cascade).
* Usage/billing, OIDC tokens, runner groups, JIT config not implemented.
* Runner: upload-artifact ignores `overwrite`/`compression-level`/`pattern`;
  checkout ignores `submodules`/`lfs`; masking does not cover encoded forms
  of secrets; docker actions ignore `pre-entrypoint`; `docker login` for
  private images uses the host's docker config; under the shell executor a
  job `container:` is ignored (warning) and services get no network alias.
* `bgh-runner run` cancels in-flight jobs on shutdown (their `always()`
  steps still run).
