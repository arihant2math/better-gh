# P26 actions-triggers — status

**In progress: self-integration pending.** Branch `bgh/p26-actions-triggers`.
Scope: `docs/PHASE4_PLAN.md` §P26 (plus the §5 Actions quick fixes moved
into P26: badge.svg and the stuck check re-run). Migration range 3800–3899
(used: `3800_actions_triggers.sql`).

## Triggers

All evaluated by the durable `actions.trigger` job; `types:` filtering
everywhere (`on.<event>.types`, default: all types, except
`pull_request[_target]` whose default stays opened/synchronize/reopened).

| Event | Activity / source | Workflows from, `GITHUB_REF` |
|---|---|---|
| `pull_request`, `pull_request_target` | opened, reopened, closed, synchronize (as before) + edited, ready_for_review, converted_to_draft, review_requested, review_request_removed (`requested_reviewer` / `requested_team`), auto_merge_enabled/disabled, and the issue-level labeled, unlabeled (`label`), assigned, unassigned (`assignee`), milestoned, demilestoned (`milestone`), locked, unlocked. Issue events on a PR never fire `issues` (as on GitHub) | PR head, `refs/pull/N/merge` / base branch |
| `pull_request_review` | submitted, edited (`changes`), dismissed; payload `review` | PR head, `refs/pull/N/merge` |
| `pull_request_review_comment` | created, edited, deleted; payload `comment` (snapshot for deleted) | PR head, `refs/pull/N/merge` |
| `create` / `delete` | branch or tag created / deleted by a push (`ref`, `ref_type`, `master_branch`); no `create` when a push creates > 3 tags | created ref / default branch |
| `issues` | + labeled, unlabeled, assigned, unassigned, milestoned, demilestoned, locked, unlocked, pinned, unpinned, transferred (fired in the old repository), deleted (snapshot) | default branch |
| `issue_comment` | + edited (`changes`), deleted (snapshot) | default branch |
| `release` | created, published, edited (`changes`), deleted (snapshot), prereleased, released, unpublished; drafts don't fire created/edited/deleted | the tag (default branch if gone) |
| `label`, `milestone` | created, edited, deleted (+ milestone opened, closed) | default branch |
| `watch` (started), `fork` (`forkee`), `public`, `gollum` (`pages`) | stars, forks, publicize, wiki writes | default branch |
| `check_run` | created, completed, rerequested, requested_action, of **other integrations'** checks (Actions' own check runs never trigger, which would loop) | default branch |
| `check_suite` | completed, of other integrations' suites | default branch |
| `workflow_run` | requested, in_progress, completed; `workflows:` names and `branches` / `branches-ignore` of the triggering run; payload `workflow_run` + `workflow` (REST shapes). Chains stop after 3 levels (GitHub's limit), so a workflow listening to itself can't loop | default branch |
| `repository_dispatch` | `POST /repos/{o}/{r}/dispatches` (`types:` match `event_type`); payload `action`, `branch`, `client_payload` | default branch |

Code: `trigger.rs` (push incl. create/delete, PR family, issues, comments,
release; arms are additive) and the new `trigger_events.rs` (`map_event`
for the new domain events, `apply_hints` expanding ids into payload objects
when the job runs, `on_repo_event` for default-branch events,
`workflow_run` depth limit).

## Pull request runs on the merge ref

* `bgh_git::merge::test_merge` (new, shared by bgh-pulls and bgh-actions):
  merge-tree of base + head, commit (parents base, head, site committer,
  dated like the head commit → deterministic sha), point
  `refs/pull/N/merge` at it; on conflict **delete** the ref and return
  `None`. bgh-pulls' mergeability refresh uses it (so the ref follows head
  and base changes and disappears while the PR conflicts).
* The trigger computes the same merge commit at trigger time (no race with
  the async refresh). `pull_request` / review runs: `GITHUB_REF =
  refs/pull/N/merge`, `GITHUB_SHA` (`github.sha`, checkout) = merge commit,
  recorded as `pull_request.merge_commit_sha` in the event payload
  (`context::run_sha`). Like GitHub, the run's `head_sha` and its check
  suite stay on the PR head commit, so the PR's checks see them.
  Conflicting PR → no `pull_request` run (`pull_request_target` still runs).
  Merged PRs (`closed`) use the real merge commit.

## Re-run from Checks

`POST /check-runs/{id}/rerequest` and `/check-suites/{id}/rerequest`
(bgh-pulls) emit `CheckRunRerequested` / `CheckSuiteRerequested`; the new
listener `actions.rerequest` → durable job `actions.rerequest`
(`rerequest.rs`): a check run re-runs its job (+ dependents) as a new
attempt, a suite re-runs the whole run. The re-run job **reuses the reset
check run** (`engine::insert_job` picks an earlier attempt's check of the
same job key + name that is `queued`; `checks::reuse_run`), so the check
moves queued → in_progress → completed instead of staying queued. A
rerequest while the run is still going restores the check from its job.

## Job concurrency

`jobs.<id>.concurrency` (string or `{group, cancel-in-progress}`; group
evaluated with github/needs/matrix/inputs/vars/strategy) via
`actions_jobs.concurrency_group` (migration 3800, partial index). One job
of a group runs; the newest waits as `pending` (an older pending one is
cancelled); `cancel-in-progress` cancels running/queued ones (durable job
`actions.cancel_job`). When a grouped job completes, `advance_run` starts
the oldest waiting job of the group (`release_job_groups`, pg advisory
xact lock per repo+group). Max-parallel promotion skips jobs waiting on a
group.

## Endpoints

| Endpoint | Notes |
|---|---|
| `POST /repos/{o}/{r}/dispatches` | `{event_type, client_payload}` → 204; write access (403 read-only, 401 anonymous, 404 no access), 422 for missing/empty or > 100 char `event_type`, non-object or > 10-key `client_payload`; archived → 403. Emits the new `Event::RepositoryDispatch` (durable): the `repository_dispatch` webhook (bgh-notify arm) and the trigger |
| `GET /{o}/{r}/actions/workflows/{file\|id}/badge.svg?branch=&event=` | SVG badge of the latest **completed** run of the workflow on `branch` (default branch when omitted), optionally only `event`: "passing" (success), "failing" (failure, timed_out, startup_failure, action_required), "cancelled", else "no status". `Cache-Control: max-age=0, no-cache, no-store, must-revalidate, private`, `Expires: 0`. Private repos need read access (404 otherwise). Index `actions_runs_badge_idx` |

## Web

* Workflow page "…" menu (now for every reader, enable/disable stays
  admin-only): **Create status badge** dialog (`BadgeDialog.tsx`) — branch
  and event selects, live preview (fetched SVG, so it also works with the
  mock), Markdown snippet + copy button.
* Run filters list the new events.
* Checks tab "Re-run" now actually re-runs (backend); the mock simulates
  queued → in_progress → completed.
* Mocks: `GET /:owner/:repo/actions/workflows/:id/badge.svg`.
* `scripts/actions-e2e.sh` / `web/scripts/actions-e2e.mjs` (real backend,
  Playwright): badge dialog + badge.svg, and re-run from a PR's Checks tab
  (same check run completes again, run attempt 2). All checks pass.

## Tests

* `crates/bgh-actions/tests/it/triggers.rs`: one test per event family —
  repository_dispatch (shape, types, validation, permissions), workflow_run
  (completed after CI, requested, 3-level chain limit), PR activity types +
  reviews + review comments + merge ref (parents base/head, check suite on
  the head), conflicting PR, create/delete, issues/issue_comment types,
  release types (incl. draft rule), label/milestone/watch/fork/public/gollum,
  check_run/check_suite (other integrations only), check rerequest re-runs
  (run → job, suite → run; same check run completes), job concurrency
  (waiting, replacing the pending job, cancel-in-progress), badge.svg
  (no status/passing/failing, branch/event filters, by id, 404, private).
* `e2e.rs`: shell executor runs a PR workflow that checks out the merge
  commit (`GITHUB_SHA`, `GITHUB_REF`, two parents, both sides' files) and a
  `repository_dispatch` workflow reading `client_payload`.
* `runs.rs` PR test updated (merge ref, run on head).
* `scripts/gh-compat.sh`: `gh api -X POST repos/{o}/{r}/dispatches` starts a
  `repository_dispatch` run.
* bgh-notify coverage: `repository_dispatch` is now producible.

## Shared-code changes (additive)

* `bgh-core/src/events.rs`: new variant `RepositoryDispatch {repo_id,
  actor_id, event_type, client_payload (serde default), branch}`.
* `bgh-git/src/merge.rs`: new `test_merge`, `remove_ref`.
* `bgh-pulls/src/mergeability.rs`: uses `test_merge` (deletes the merge ref
  on conflict).
* `bgh-notify/src/payloads/mod.rs`: `repository_dispatch` arm.
* `bgh-actions`: `checks::reuse_run`, `context::run_sha`,
  `engine::{CancelJob, cancel_job_job}`, new modules `trigger_events`,
  `rerequest`, `badge`, `api::dispatches`.

## Known gaps

* `deployment` / `deployment_status`, `merge_group`, `discussion*`,
  `registry_package`, `branch_protection_rule`, `status`, `page_build`,
  `project*` triggers are still not fired (P19/P20/P39/P56 add the
  domain features; each needs one `map_event` arm).
* The GITHUB_TOKEN loop guard (events caused by a job token don't start
  workflows) belongs to P8; `workflow_run` / `repository_dispatch` are the
  documented exceptions on GitHub.
* `release` `published` for a non-draft create is followed by GitHub's
  `released`; only the events bgh-releases emits are mapped.
* Badge text width is approximated (no font metrics).
