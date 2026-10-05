# P19 — Deployments API and deployment statuses — status

**Done.** Branch `bgh/p19-deployments`, self-integrated into
`claude/sleepy-cray-9jj0t3`. Migration `3100_deployments.sql` (range
3100–3199).

## Endpoints (REST, relative to `/api/v3`)

| Method + path | Who | What |
|---|---|---|
| `GET /repos/{o}/{r}/deployments` | readers | newest first, `Link` pagination; filters `sha`, `ref`, `task`, `environment` (case-insensitive) |
| `POST /repos/{o}/{r}/deployments` | writers (403 readers, 404 no access, 403 archived) | `ref` (required: branch, tag, `refs/…` or SHA; 422 `No ref found for: X`), `task` (`deploy`), `auto_merge` (default true), `required_contexts`, `payload` (object, or a string; a JSON-object string is stored as the object), `environment` (`production`), `description`, `transient_environment` (false), `production_environment` (default `environment == "production"`) → 201 deployment |
| `GET /repos/{o}/{r}/deployments/{id}` | readers | GitHub `deployment` shape (`url, id, node_id, sha, ref, task, payload, original_environment, environment, description, creator, created_at, updated_at, statuses_url, repository_url, transient_environment, production_environment, performed_via_github_app`) |
| `DELETE /repos/{o}/{r}/deployments/{id}` | writers | 204 when the latest status is `inactive` or it is the only deployment in its environment; else 422 "We cannot delete an active deployment unless it is the only deployment in a given environment." |
| `GET /repos/{o}/{r}/deployments/{id}/statuses` | readers | newest first, `Link` pagination |
| `POST /repos/{o}/{r}/deployments/{id}/statuses` | writers | `state` (required, one of `error failure inactive in_progress queued pending success`, else 422), `target_url`/`log_url` (each defaults to the other), `description` (truncated to 140), `environment` (moves the deployment; `original_environment` keeps the first), `environment_url`, `auto_inactive` (default true) → 201 status |
| `GET /repos/{o}/{r}/deployments/{id}/statuses/{status_id}` | readers | `deployment_status` shape (`url, id, node_id, state, creator, description, environment, target_url, created_at, updated_at, deployment_url, repository_url, environment_url, log_url, performed_via_github_app`) |

Node ids: `NodeType::Deployment` / `DeploymentStatus` (legacy format).
The repository JSON's `deployments_url` now resolves.

### Creation semantics

* **auto_merge** (default true): when `ref` names a branch other than the
  default branch and the default branch is not an ancestor of it, the
  default branch is merged into it (merge commit by the caller, written
  through `bgh_repos::refs::write_ref`, so branch protection and rulesets
  apply) and the API answers **202** `{"message": "Auto-merged main into
  topic on deployment."}` without creating a deployment (GitHub's
  behaviour; the client retries). Conflicts → **409** "Conflict merging
  main into topic.". Tags, SHAs and the default branch are never merged.
* **required_contexts**: omitted = every status context and check run
  name reported on the commit; `[]` skips the check. Latest commit status
  per context; check runs count as success when completed with
  `success`/`neutral`/`skipped`. Any non-success (or missing) context →
  **409** `{"message": "Conflict: Commit status checks failed for <ref>.",
  "errors": [{"contexts": [{"context", "state"}], "resource": "Deployment",
  "field": "required_contexts", "code": "invalid"}]}` (missing contexts
  have state `missing`).
* The environment is auto-created in `actions_environments` (so it gets
  secrets/variables/`html_url` like any environment).
* **auto_inactive**: a new `success` status adds an `inactive` status to
  every other deployment of the same repository and environment whose
  latest state is `success` (each emits its own `deployment_status`
  event).

## Events, webhooks, timeline, merge box

* `Event::DeploymentCreated { repo_id, deployment_id, actor_id }` and
  `Event::DeploymentStatusCreated { repo_id, deployment_id, status_id,
  state, actor_id }` (bgh-core `events.rs`, additive).
* Webhooks (bgh-notify `payloads/deployments.rs`): `deployment` created
  (`deployment, workflow: null, workflow_run: null, repository, sender`)
  and `deployment_status` created (`check_run: null, deployment,
  deployment_status, workflow: null, workflow_run: null, repository,
  sender`). `event_names` maps both; both are sampled in P10's coverage test
  (`bgh-notify/tests/it/coverage.rs`) and removed from its
  not-producible list.
* `deployed` issue event (bgh-issues listener `issues.deployed`): on a
  `success` status, every PR of the repository whose `head_sha` is the
  deployed commit gets one `deployed` event per deployment (actor = the
  deployment creator, `commit_id` = SHA, data `{deployment_id,
  environment}`; REST shows no extra fields, like GitHub). Idempotent
  (`claim_effect` + a NOT EXISTS guard). The web timeline renders it with
  its generic fallback ("deployed"); a richer row is Timeline-owner work.
* Merge box data: `GET /_bgh/repos/{o}/{r}/pulls/{n}/requirements` has a
  new `deployments` array (latest deployment of the head commit per
  environment: `deployment_id, environment, state, environment_url,
  log_url, production_environment, transient_environment, updated_at`).
  MergeBox.tsx is untouched (P74 owns it); the PR page renders a separate
  `DeploymentsBanner` above it ("This branch was successfully deployed")
  that reads the same cached resource (same key as MergeBox, so no extra
  request). Open PRs only (the requirements resource is only loaded for
  open PRs).

## Web

* `/:owner/:repo/deployments` and `/:owner/:repo/deployments/activity_log`
  (`?environments_filter=` — the environment JSON's `html_url`):
  environment cards (latest state, ref/SHA, time, deployment count, "View
  deployment"), activity log (state pill, environment, production /
  transient / task tags, ref + SHA, creator, status description, View
  deployment / Logs links, lazily loaded status **History** via REST),
  environment filter (URL query), "Load more" paging. Data:
  `GET /_bgh/repos/{o}/{r}/deployments?environment=&page=` →
  `{environments: [{id, name, deployments, latest}], deployments: [row],
  page, hasMore, canWrite}` (rows camelCase: `id, environment, ref, sha,
  task, description, state, creator{login, avatarUrl},
  productionEnvironment, transientEnvironment, createdAt, updatedAt,
  environmentUrl, logUrl, statusDescription`; 30 per page).
* Repository home sidebar: "Deployments" section (environments with a
  deployment and their latest state; hidden when none).
* Mock backend: `web/src/mock/deployments.ts` (summary, REST list/get/
  create, statuses list/create with auto_inactive, merge-box
  `deployments`), test `deployments.test.ts`.
* Verified with Playwright in mock mode (light + dark) and against a real
  `bgh serve` (REST-seeded deployments, login, sidebar, page, history,
  filter, PR banner).

## Tables

* `deployments(id, repo_id, environment_id → actions_environments SET
  NULL, environment, original_environment, sha, ref, task, payload JSONB,
  description, creator_id, transient_environment, production_environment,
  state /* latest status, NULL before the first */, latest_status_id,
  run_id → actions_runs, job_id → actions_jobs, created_at, updated_at)`;
  indexes `(repo_id, id DESC)`, `(repo_id, lower(environment), id DESC)`,
  `(repo_id, sha)`, plus FK indexes.
* `deployment_statuses(id, deployment_id CASCADE, repo_id, state,
  description, environment, target_url, log_url, environment_url,
  creator_id, created_at, updated_at)`; index `(deployment_id, id DESC)`.

## Shared-code changes (additive)

* `bgh_core::deployments` (new): `DeploymentRow`, `DeploymentStatusRow`
  (+ `find`), `deployment_json`, `status_json` (GitHub shapes shared by
  REST and webhooks), `STATES`, `latest_for_sha` /
  `EnvironmentDeployment`.
* `bgh_core::node_id::NodeType::{Deployment, DeploymentStatus}`.
* `bgh_core::events::Event::{DeploymentCreated, DeploymentStatusCreated}`.
* bgh-actions now depends on bgh-repos (git CLI handle, `write_ref`,
  `default_identity` for auto_merge). bgh-repos does not depend on
  bgh-actions, so there is no cycle.
* bgh-pulls `web::Requirements.deployments`; bgh-issues `deployed`
  listener + REST arm.

## Extension points for P20 (environment protection rules)

* `bgh_actions::deployments` is the service every writer must use:
  `create_deployment(tx, repo_id, creator_id, NewDeployment)` and
  `create_status(tx, repo_id, deployment_id, creator_id, NewStatus)` (both
  inside the caller's `Tx`; they emit the events and apply auto_inactive).
  A job with `environment:` should call `create_deployment` with
  `run_id`/`job_id` set, then `create_status` with `in_progress` →
  `success`/`failure` and `environment_url` from `environment.url`.
* `ensure_environment(tx, repo_id, name)` is the single place deployments
  create environments; protection rules hang off `actions_environments`
  (add columns/tables in P20's range).
* `deployments.run_id/job_id` link a deployment to its job (for
  `pending_deployments`, `on: deployment` triggers and approvals).
* `required_deployments` (P20, bgh-pulls evaluator): use
  `bgh_core::deployments::latest_for_sha(db, repo_id, head_sha)` and
  require `state == "success"` for each listed environment.
* Branch policies: check them in the API `create` handler (before
  `create_deployment`) and in the engine; the handler already resolves the
  ref to a SHA and knows whether it is a branch.

## Known gaps / TODO

* Token scope `repo_deployment` is not special-cased (plain repository
  permissions apply; see AUDIT "Narrow scopes … ineffective").
* GraphQL `Repository.deployments`/`environments` and the
  `DeployedEvent` timeline item are not implemented (P44/P45).
* No sync model for deployments: the web page reads REST/`_bgh` with a
  short cache TTL (15 s) instead of live deltas.
* Deployments created by Actions jobs (`environment:`) and the
  `on: deployment` / `deployment_status` triggers are P20.
