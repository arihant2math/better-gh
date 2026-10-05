# F8 actions-web — status

Branch `bgh/actions-web`. Web UI for GitHub Actions (runs, run graph, live
logs, dispatch, settings) plus a small backend completion.

**Status:** complete — scope done, merged with the integration branch, verified end to end against the real backend (`scripts/actions-e2e.sh`).

## Backend (additive)

* `Event::WorkflowJobUpdated` gained `workflow_job: serde_json::Value`
  (`#[serde(default)]`): the job as GitHub REST JSON, same builder as
  `GET /actions/jobs/{id}` (`bgh_actions::engine::job_event`). Emitted for
  `queued`, `in_progress`, `completed` (and `waiting` when produced).
* bgh-notify maps it to `workflow_job` webhooks (`event_names` +
  `payloads::for_event`); `null` payloads (older producers) deliver nothing.
* bgh-actions now also writes job annotations to `check_run_annotations`
  (one batched `UNNEST` insert in `checks::complete_run`) so
  `GET /check-runs/{id}/annotations` (bgh-pulls) serves them; previously they
  were only in `check_runs.output` (integration gap). Test in `tests/e2e.rs`.
* `bgh_core::db::Tx::state()` accessor (needed to render JSON inside a tx).
* Private UI endpoints in `crates/bgh-actions/src/ui.rs` (merged into
  `bgh_actions::web_router`):
  * `GET /_bgh/actions/repos/{o}/{r}/runs/{id}/graph` →
    `{run_id, workflow_name, jobs:[{key,name,needs,matrix,uses}], job_keys:{"<jobId>": key}}`
    (from the stored workflow definition; read access, 404 otherwise).
  * `GET /_bgh/actions/repos/{o}/{r}/workflows/{id|file}/dispatch?ref=` →
    `{ref, sha, path, dispatchable, inputs:[{name,description,required,default,type,options}], error}`
    (workflow file parsed at `ref`, default branch when omitted).
* Tests: `bgh-actions/tests/runs.rs::events_carry_webhook_payloads` (job
  JSON on queued/in_progress/completed), `bgh-actions/tests/ui.rs`,
  `bgh-notify/tests/payloads.rs::workflow_job_payloads`,
  `bgh-notify/tests/webhooks.rs::workflow_job_events_are_delivered`.

## Web

Routes (all lazy chunks under `web/src/pages/actions/`):

| Path | Page |
|---|---|
| `/:o/:r/actions`, `/:o/:r/actions/workflows/:file` | `RunsPage`: workflows sidebar, runs list |
| `/:o/:r/actions/runs/:id[/attempts/:n]` | `RunPage`: summary, job graph, annotations, artifacts |
| `/:o/:r/actions/runs/:id/job/:job` | `JobPage`: run shell + `log/JobLogView` |
| `/:o/:r/settings/secrets/actions`, `…/settings/variables/actions`, `…/settings/actions/runners`, `/:o/:r/actions/runners`, `…/settings/environments` | `settings/ActionsSettingsPage` |
| `/organizations/:org/settings/{secrets/actions,variables/actions,actions/runners}` | same, org scope |

Data: `api/actions.ts` (typed REST + private endpoints), `pages/actions/data.ts`
(cache keys + loaders + route prefetch), `pages/actions/live.ts` (observable
`runs` / `jobs` maps).

**Live updates without refetch storms.** The server already records
`workflow_run` / `workflow_job` sync actions (scope `repo:{id}`); the web
client now exposes every socket delta through `SyncClient.onDeltas()`
(additive; models not in the store schema are otherwise dropped). `live.ts`
patches the matching run/job in place (one row re-renders); an unknown id (new
run, job materialized after its `needs`, new attempt) triggers one debounced
(400 ms) refetch of the mounted view. Polling is only a fallback: every 4 s
while something runs and the socket is not live, 30 s safety net when live.
Logs stream over SSE (`/_bgh/actions/jobs/{id}/logs/stream`) read through
`transport().fetch` (works with the mock).

### Pages

* **Runs** (`RunsPage`, `RunRow`, `WorkflowsSidebar`, `DispatchPanel`): all
  workflows + per workflow (`/actions/workflows/ci.yml`), disabled workflows
  struck through, enable/disable menu (admin), "Run workflow" popover only for
  workflows with a `workflow_dispatch` trigger on the default branch (probe via
  the dispatch-form endpoint): branch picker, inputs typed as string / number /
  boolean (checkbox) / choice (select) / environment (select of repo
  environments), required validation, navigates to the new run. Filters
  event / status / branch / actor in the URL query; total count; virtualized
  list with infinite paging (50/page); j/k/Enter; command palette entries.
* **Run** (`RunPage`, `RunShell`, `RunGraph`, `graphLayout.ts`): jobs sidebar
  (matrix jobs grouped, creation order), summary facts (trigger, actor, PR,
  branch, commit, status, live duration, attempt), job DAG — layered layout
  by `needs` depth with parent-barycenter ordering, HTML nodes over an SVG edge
  layer, matrix groups (8 rows + "and N more"), pending placeholders for jobs
  not materialized yet; annotations of completed jobs (errors first,
  batched 6 at a time, ≤ 30 jobs); artifacts with size / expiry and download
  through the signed redirect; re-run all / failed, cancel, force-cancel,
  download log archive; attempt picker (`/attempts/:n`).
* **Job log** (`log/`): see the module headers. SSE stream (reconnect with
  backoff, reset on reconnect), incremental append-only parser (timestamps,
  `##[group]`/`::group::`, error/warning/notice/debug/command), ANSI SGR
  (16 colors as theme tokens, 256/truecolor), one virtualized list for all
  steps (fixed 20 px rows; tested at 100k+ lines), collapsible steps (failed /
  running expanded) and groups, step timings, search across everything with
  i / n navigation that expands steps and groups, line permalinks
  `#step:N:L[-M]`, follow-tail toggle (auto-off on scroll up), timestamps
  toggle, raw download. Rendering batched per animation frame.
* **Settings** (`settings/`): repo + org secrets (values never fetched;
  libsodium sealed box in the browser — X25519/XSalsa20-Poly1305 via
  `tweetnacl` + own BLAKE2b, lazy-loaded only when saving), env secrets,
  variables (repo / env / org, all pages), org visibility incl. selected
  repositories, runners (status, busy, labels add/remove, remove runner,
  registration token + `bgh-runner register` instructions), environments
  (create / delete, per-env secrets & variables). Admin-only; 403/404 show an
  empty state.

### Mock mode

`web/src/mock/actions.ts` serves every endpoint of `api/actions.ts` (incl.
the private ones and an SSE stream as a real `ReadableStream`), seeds 4
workflows and ~150 runs per repo (matrix/needs graphs, annotations,
artifacts, a 100k-line log), and simulates live runs (dispatch, re-run,
cancel) emitting the same `workflow_run` / `workflow_job` deltas as the
server. `server.ts` gained `Resp.stream` and `recordRaw()` (additive).

### Shared web changes (additive)

* `SyncClient.onDeltas(fn)`; `api/cache.refresh(key, loader)`; icons added to
  `ui/icons.ts`; routes in `app/routes.ts` (Actions routes before
  `/:owner/:repo/:tab`, org settings before `/:owner`); `actions` removed from
  `RepoPlaceholderPage`.
* Dependency `tweetnacl@1.0.3` (only in the lazy `sealedBox` chunk).
* `scripts/lib/test-server.sh`: `TS_EXTRA_ENV` to add/override server env.

### Bundle

Initial JS 121.2 KB gzip (+~2 KB: routes, delta hook). Lazy chunks:
JobPage 9.3 KB, ActionsSettingsPage 10.9 KB, sealedBox 11.7 KB, RunsPage
5.9 KB, RunPage 4.4 KB, RunShell 3.8 KB (gzip).

## Verification

* `npm run typecheck && npm run lint && npm test && npm run build`: green
  (unit tests: graph layout, live store, ANSI / log parser / SSE parser incl.
  100k-line search, sealed box + BLAKE2b vectors, mock actions backend).
* `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo test --workspace`: green.
* `scripts/actions-e2e.sh [--shots DIR]` (needs `npm run build` first):
  throwaway server serving `web/dist`, built-in runner on the shell executor,
  pushes a repo with a CI workflow (lint → build matrix → test, a failing job
  with an `::error` annotation, a skipped deploy, artifacts), then Playwright:
  runs list, run graph, annotations, artifact download, failed log, group
  search, **secret created through the UI decrypted by a job** (sealed box
  end to end), dispatch with inputs, **log lines streaming live**, run turning
  green without reload, re-run failed jobs → attempt 2, runner registration
  token, dark theme. 17/17 checks pass; screenshots written to `--shots`.

## Known gaps / TODO

* Checks tab deep links: check runs' `details_url` is the job page
  (`/{o}/{r}/actions/runs/{run}/job/{job}`), which this package routes; the
  PR Checks tab (F4, not yet integrated) opens it in a new tab — switching it
  to in-app `navigate()` is a one-line change once both are merged.
