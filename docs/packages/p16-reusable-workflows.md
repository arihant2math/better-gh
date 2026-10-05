# P16 — Reusable workflows (`jobs.<id>.uses` / `on.workflow_call`): status

Branch `bgh/p16-reusable-workflows`. Migration
`2800_actions_reusable_workflows.sql` (range 2800–2899).

**Status:** integrated into `claude/sleepy-cray-9jj0t3` (full gate,
`actions-e2e.sh`, `gh-compat.sh`, `api-smoke.sh` green). One scope item is
outstanding: the `permissions:` intersection, which needs P8's
`bgh_core::token_permissions` (not on the integration branch yet); the caps
are already stored per job (see Known gaps).

## How it works

* `crates/bgh-actions/src/reusable.rs`: `parse_ref` (`./.github/workflows/x.yml`
  or `owner/repo/.github/workflows/x.yml@ref`, validated at parse time too),
  `resolve` (reads and parses the file, checks `on.workflow_call`),
  `may_call` (access rule), `typed_inputs`, `secrets_layer`,
  `apply_secret_layers`, `outputs`, `root_key`.
* Engine (`engine.rs`): a calling job no longer hard-fails. Per matrix
  combination it inserts a **`call` row** (`actions_jobs.kind = 'call'`,
  `spec` = `reusable::StoredCall`: called workflow definition, typed inputs,
  secret layers, permission caps, key/name prefixes, depth, source repo and
  sha). `advance_run` schedules the run's workflow plus one **scope** per
  in-progress call: the called jobs are ordinary job rows keyed
  `<call>/<job>` (`<call>.<matrix index>/<job>`, nested calls add segments)
  and named `<caller> / <job>` (check runs too). When every job of a scope
  completed, the call row completes with the aggregated result and the
  evaluated `on.workflow_call.outputs` (`jobs` context), so
  `needs.<caller>.outputs/result` work downstream. fail-fast / max-parallel
  apply to matrix callers (pending call rows); cancelling a call cancels its
  jobs (and nested calls).
* Call rows have no check run, logs, sync rows or webhooks; they are hidden
  from the REST jobs list/count, `GET /actions/jobs/{id}` (404), the logs
  zip and the stale-job reaper.
* Resolution: `./` at the caller's repository and commit (nested calls
  resolve against the called workflow's repository/commit); remote refs try
  `refs/heads/<ref>`, `refs/tags/<ref>`, then a SHA. Access: same repo, any
  public repo, a private/internal repo of the same owner whose access level
  is `user`/`organization`/`enterprise`, or any internal repo with
  `enterprise`.
* `with:` is kept as raw YAML values (`Job.with` is now
  `IndexMap<String, Value>`), evaluated with the caller's context (github,
  needs, inputs, vars, matrix, strategy) and typed against the declared
  inputs: unknown input, missing required input, non-boolean/non-number
  values are errors; defaults apply; numbers/booleans given to string inputs
  are stringified.
* `secrets:`: `inherit`, or a mapping validated against
  `on.workflow_call.secrets` (unknown name, missing required secret). The
  mapping expressions are evaluated when a runner claims the job
  (`server::prepare_spec`) against the previous layer's secrets and the
  caller's matrix/needs/inputs/strategy, so secrets are never stored. A
  called job's own `environment:` secrets are added on top.
* Contexts: `github` stays the caller's (`workflow`, `workflow_ref` of the
  top-level file, `event`, `sha`...); `github.job` is the called job id;
  `github.job_workflow_sha` is the commit of the called workflow; `inputs.*`
  are the typed inputs.
* Caller `concurrency` (job level): the call row waits as `pending` while
  another active row holds the group (newer pending rows replace older
  ones, `cancel-in-progress` cancels the holder through the durable
  `actions.cancel_job` job); finishing a call wakes waiting runs.
* Limits: nesting depth 4 (`reusable::MAX_DEPTH`) and 20 unique called
  workflows per run (`MAX_UNIQUE_WORKFLOWS`).
* Errors (bad ref syntax at run time, missing workflow or ref, no access,
  missing `on.workflow_call`, input/secret errors, limits) fail the calling
  job: a failed job row named after the caller, with the message as its log
  (`##[error]…`) and a failure annotation on its check run (path = the
  workflow file containing the call).
* Re-runs: re-running any job of a call (or a failed one) re-runs the whole
  top-level calling job (`reusable::root_key`).

## Endpoints

* `GET|PUT /repos/{o}/{r}/actions/permissions/access` — GitHub's
  `{"access_level": "none" | "user" | "organization" | "enterprise"}`; admin
  only; 422 for unknown levels, public repositories, and `enterprise` on
  non-internal repositories. (P30 owns the rest of `/actions/permissions`.)
* `GET /_bgh/actions/repos/{o}/{r}/runs/{id}/graph` gains `calls` (latest
  attempt: id, key, root, prefix, name, uses, workflow_ref, status,
  conclusion, called jobs with prefixed keys/needs); `job_keys` lists job
  rows only.

## Tables / migration 2800

* `actions_jobs.kind` (`job` | `call`), `actions_jobs.concurrency_group`
  (+ partial index on active grouped rows).
* `actions_repo_access (repo_id PK, access_level, updated_at)`.

## Web

* `pages/actions/calls.ts` (`rootKey`, `pendingCalled`, `calledLabel`, unit
  tested in `calls.test.ts`). The run graph renders a calling job as a group
  node (header: caller name + called workflow file, rows: its called jobs,
  pending rows for called jobs not created yet), edges attach to the caller.
  The jobs sidebar groups called jobs under the caller.
* Mock: the Deploy workflow's staging/production deploys are reusable calls
  (`deploy-env.yml`); the mock graph endpoint returns `calls`.

## Tests

* `crates/bgh-actions/tests/it/reusable.rs` (shell executor, end to end):
  local call with typed inputs, outputs consumed downstream, contexts, check
  run names, graph `calls`, single-job re-run of a called job; matrix
  caller + `secrets: inherit` + mapped secrets; cross-repo call (denied,
  then allowed through the access endpoint; other owners denied); errors
  (missing workflow, missing ref, input type, unknown input, not reusable,
  depth limit; dependent skipped); 20-unique limit; caller concurrency and
  cancel cascade.
* Unit tests in `reusable.rs` (ref parsing, input typing, secret layers,
  outputs) and the workflow parser test (`with` raw values).
* `scripts/actions-e2e.sh`: CI calls `./.github/workflows/package.yml`; the
  Playwright driver checks the call node, its called jobs, the sidebar and
  the downstream job consuming the outputs.

## Shared-code changes

None outside `bgh-actions` (migration only).

## Known gaps

* `permissions:` intersection: caps are stored per hop
  (`StoredJob.permission_caps: Vec<Option<Permissions>>`, `None` = the
  default) but not applied until P8 lands. Planned in `server::prepare_spec`:
  base = the called job's own permissions, else the innermost cap, else the
  default; then intersect with every cap (`None` → site default).
* `runs::pull_request_events_trigger_runs` (pre-existing) failed once under
  full-workspace load (settle timing); it passed 4/4 in isolation and in the
  final gate.
* Job-level `concurrency` is only enforced for calling jobs (normal jobs:
  P26). The called workflow's own top-level `concurrency:` is ignored.
* Run-name / `on.workflow_call` validation happens when the calling job
  becomes ready, not at run start (GitHub reports some errors as a
  startup failure instead).
* Re-running a single called job re-runs the whole calling job.
