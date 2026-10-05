# F8 actions-web — status

Branch `bgh/actions-web`. Web UI for GitHub Actions (runs, run graph, live
logs, dispatch, settings) plus a small backend completion.

**Status:** in progress.

## Backend (additive)

* `Event::WorkflowJobUpdated` gained `workflow_job: serde_json::Value`
  (`#[serde(default)]`): the job as GitHub REST JSON, same builder as
  `GET /actions/jobs/{id}` (`bgh_actions::engine::job_event`). Emitted for
  `queued`, `in_progress`, `completed` (and `waiting` when produced).
* bgh-notify maps it to `workflow_job` webhooks (`event_names` +
  `payloads::for_event`); `null` payloads (older producers) deliver nothing.
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

(Sections below are filled in as parts land.)

## Known gaps / TODO

* Checks tab deep links: check runs' `details_url` is the job page
  (`/{o}/{r}/actions/runs/{run}/job/{job}`), which this package routes; the
  PR Checks tab (F4, not yet integrated) opens it in a new tab — switching it
  to in-app `navigate()` is a one-line change once both are merged.
