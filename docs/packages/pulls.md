# B4 pulls — status

Branch `bgh/pulls`, crate `bgh-pulls` (+ `bgh-git` `merge`/`patch` modules),
migration `0400_pulls.sql`. **Status: complete** (scope of WORKPLAN B4);
26 integration tests (`crates/bgh-pulls/tests/`) with real git repos.

## Endpoints (relative to `/api/v3`, GitHub REST shapes)

Pulls
* `GET/POST /repos/{o}/{r}/pulls` — list (state, head `user:ref`, base,
  sort created|updated|popularity|long-running, direction, pagination);
  create (same-repo or cross-fork `owner:branch`, `head_repo`, draft,
  maintainer_can_modify, `issue` → converts an issue). 201 + `Location`.
* `GET/PATCH /repos/{o}/{r}/pulls/{n}` — full `pull-request` shape
  (`mergeable`/`rebaseable` `null` + `mergeable_state: unknown` until the
  background job ran); `Accept: application/vnd.github.diff|.patch` streams
  `git diff` / `git format-patch` (406 over 300 files / 20 000 lines).
  PATCH: title, body, state (close/reopen), base, maintainer_can_modify.
* `GET /pulls/{n}/commits` (≤ 250, paginated with `last`), `GET
  /pulls/{n}/files` (status added/removed/modified/renamed/changed, patch
  hunks, `previous_filename`, ≤ 3000 files, patches > 1 MiB omitted).
* `GET /pulls/{n}/merge` (204/404), `PUT /pulls/{n}/merge` (merge | squash |
  rebase, `sha` check → 409, commit_title/message, repo merge settings,
  405 messages like GitHub), `PUT /pulls/{n}/update-branch`
  (`expected_head_sha`, 202; pushes into forks when maintainer edits are
  allowed).
* `GET /repos/{o}/{r}/commits/{sha}/pulls`.

Reviews / comments / reviewers
* `GET/POST /pulls/{n}/reviews`, `GET/PUT/DELETE /pulls/{n}/reviews/{id}`,
  `POST …/{id}/events` (submit), `PUT …/{id}/dismissals`,
  `GET …/{id}/comments`. Pending reviews (and their comments) are visible
  only to their author; one pending review per user; can't approve /
  request changes on your own PR; submitting removes the reviewer's
  request.
* `GET /repos/{o}/{r}/pulls/comments` (sort, direction, since),
  `GET/PATCH/DELETE /pulls/comments/{id}`, `GET/POST /pulls/{n}/comments`
  (line + side, multi-line start_line/start_side, legacy `position`,
  `subject_type: file`, `in_reply_to`), `POST
  /pulls/{n}/comments/{id}/replies`, reactions
  (`GET/POST /pulls/comments/{id}/reactions`, `DELETE …/reactions/{rid}`).
  `position`/`original_position`/`diff_hunk` computed from the PR diff;
  standalone comments get their own COMMENTED review (like GitHub).
* `GET/POST/DELETE /pulls/{n}/requested_reviewers` (users must be
  collaborators — public visibility doesn't count; teams of the owning org
  with access to the repo).

Statuses / checks
* `POST /statuses/{sha}`, `GET /statuses/{ref}`, `GET /commits/{ref}/statuses`,
  `GET /commits/{ref}/status` (combined).
* `POST /check-runs`, `GET/PATCH /check-runs/{id}`, `GET
  /check-runs/{id}/annotations`, `POST /check-runs/{id}/rerequest`,
  `GET /commits/{ref}/check-runs` (check_name, status, filter latest|all,
  app_id), `POST /check-suites`, `GET /check-suites/{id}`,
  `GET /check-suites/{id}/check-runs`, `POST /check-suites/{id}/rerequest`,
  `PATCH /check-suites/preferences`, `GET /commits/{ref}/check-suites`.
  The `app` object is synthesized: slug `api` (id 2) for REST-created runs,
  `actions` (id 1) for bgh-actions.

`/_bgh` (GraphQL-only on GitHub; service fns are `pub` for bgh-graphql)
* `GET /_bgh/repos/{o}/{r}/pulls/{n}/threads`, `POST
  …/threads/{root_comment_id}/resolve|unresolve`
  (`web::set_resolved`).
* `POST …/ready_for_review`, `POST …/convert_to_draft` (`web::set_draft`).
* `PUT/DELETE …/auto_merge` `{merge_method, commit_title, commit_message}`
  (`automerge::enable/disable`; needs `allow_auto_merge`).
* `GET …/requirements` — merge box data (blockers, approvals, required
  checks, allowed methods, admin bypass).

## Behaviour

* Head mirroring: every PR's head is kept at `refs/pull/{n}/head` in the
  base repo (fetched from forks), the test merge commit at
  `refs/pull/{n}/merge` (`merge_commit_sha` while open, deterministic).
* Mergeability (`pulls.refresh` job): `git merge-tree --write-tree`
  conflicts → `dirty`; rebaseability by replaying commits (≤ 100);
  `mergeable_state` = draft | dirty | behind | blocked | unstable | clean
  from branch protection. Re-run on create, push to head/base, base change,
  reviews, statuses, check runs, thread resolution, draft toggles; then
  auto-merge is attempted (waits for all requirements and pending checks).
* Merges never use a work tree: merge-tree → commit-tree → `update-ref`
  with old value (409 if the base moved). Squash commits are authored by
  the PR author; rebase preserves authors. After merging, a
  `repos.post_receive` job is enqueued (pushed_at, `Event::Push`).
  `delete_branch_on_merge` deletes same-repo heads (not default/protected/
  used by other open PRs) and retargets dependent PRs to the base.
* Branch protection (`branch_protections`, all matching patterns combine):
  required approving reviews (write+ reviewers, author excluded, latest
  decisive review per user), changes requested, code owner reviews,
  required status checks (statuses **and** check runs, `contexts` and
  `checks[].context`, strict → `behind`), conversation resolution, linear
  history, lock_branch, enforce_admins (admins bypass otherwise).
  `dismiss_stale_reviews` dismisses approvals on new commits.
* Synchronize (`Event::Push` → `pulls.push` job → `synchronize`): new head
  mirrored, force-push detection (`head_ref_force_pushed`), deleted /
  restored head branches, base moves (stats recomputed), PRs merged outside
  (head reachable from base → marked merged), review comments remapped to
  the new diff or outdated (`position`/`line` = null), CODEOWNERS
  re-request, `PullRequestSynchronized`.
* CODEOWNERS: `.github/CODEOWNERS`, `CODEOWNERS`, `docs/CODEOWNERS` on the
  base; `@user`, `@org/team`, emails; gitignore-like patterns, last match
  wins; owners need write access (teams need a `team_repos` grant).
  Auto-requested on open (non-draft), ready-for-review and pushes.
* Diffs: parsed file diffs cached in Redis by `(repo, base, head)` for 7
  days (`pulls:diff:v1:*`); `.diff`/`.patch` streamed from git.

## Sync & events

Sync rows follow `docs/SYNC_PROTOCOL.md` (scope `repo:{id}`, camelCase):
PRs are `issue` rows with `isPr: true` and every PR field of the v1
interface (`reviewDecision` from the latest decisive reviews / pending
requests, `checks` aggregated from statuses + check runs on the head;
`body` only on create/body edits) plus extensions (`mergeCommitSha`,
`rebaseable`, `maintainerCanModify`, `autoMerge`, `reviewComments`);
`review` (normative shape), `issueEvent` (`data` mapped to camelCase:
`reviewerId`, `teamId`, `from`/`to`, `before`/`after`, `reviewId`,
`commitId`, ...). Extension models (ignored by today's client):
`reviewComment`, `reaction`, `commitStatus`, `checkRun`, `checkSuite`.
Pending reviews are not broadcast. The `repo` row's `openPulls` is not
re-synced by this crate (repo rows belong to bgh-repos).
Event variants added (additive, `events.rs`):
`PullRequestEdited`, `PullRequestReadyForReview`,
`PullRequestConvertedToDraft`, `PullRequestReviewRequested`,
`PullRequestReviewRequestRemoved`, `PullRequestReviewEdited`,
`PullRequestReviewDismissed`, `PullRequestReviewComment{Created,Edited,Deleted}`,
`PullRequestReviewThread{Resolved,Unresolved}`,
`PullRequestAutoMerge{Enabled,Disabled}`, `CommitStatusCreated`,
`CheckRun{Created,Completed,Rerequested}`,
`CheckSuite{Requested,Rerequested,Completed}`. A merge emits
`PullRequestMerged` (not an extra `PullRequestClosed`).

Timeline events are rows in `issue_events` (rendered by bgh-issues); names
and `data` keys are documented in `crates/bgh-pulls/src/timeline.rs`
(`review_requested {requested_reviewer_id|requested_team_id}`,
`review_dismissed {dismissed_review}`, `merged`/`closed` with `commit_id`,
`head_ref_force_pushed {before, after}`, `head_ref_deleted|restored`,
`base_ref_changed {from, to}`, `renamed {rename}`, `convert_to_draft`,
`ready_for_review`, `auto_merge_enabled|disabled`).

## Web client wiring

`web/src/pages/pulls/PullDetailPage.tsx`: Files tab uses the `.diff` media
type (cache key includes base/head SHAs; 406 → "too large" state), Commits
tab uses `/pulls/{n}/commits`, the merge box reads
`/_bgh/.../requirements` (blockers, approvals, required checks, allowed
merge methods with a method picker, admin bypass) and merges via
`PUT /pulls/{n}/merge`. `setDraft` now calls the `/_bgh` ready_for_review /
convert_to_draft endpoints (the mock backend implements them too).

## Schema (0400_pulls.sql)

`pr_reviews`: one-pending-per-user unique index, `dismissed_at`,
`dismissal_message`. `pr_requested_reviewers.as_code_owner` + indexes.
`check_runs.actions`, `check_runs.creator_id`; `check_suites.rerequestable`,
`latest_check_runs_count`; new `check_run_annotations`. Indexes for
`merge_commit_sha`, auto-merge candidates, review comment replies.

## Shared-code changes

* `bgh-core`: event variants above; `NodeType::PullRequestReviewThread`.
* `bgh-git`: new `merge` (merge-base, merge-tree, commit-tree, rev-list,
  rebase, cross-repo `fetch_ref`/`push_ref`, ref CAS) and `patch` (diff
  files, streamed diff/patch, patch line positions) modules; `cmd`
  `run_status`/`spawn_stdout` helpers. **Fix** in `write::commit_changes`:
  file deletions used `update-index --force-remove`, which fails in bare
  repos; now uses `--index-info` with mode 0.
* `bgh-pulls` depends on `bgh-repos` for `protection::pattern_matches` and
  the `repos.post_receive` job payload.

## Known gaps / TODO

* `require_last_push_approval` is parsed but not enforced.
* HTML `diff_url`/`patch_url` (`/{o}/{r}/pull/{n}.diff`) are not served
  (the API media types are).
* `body_html`/`body_text` media types for reviews/comments not rendered.
* Comments created against an older `commit_id` are positioned on that
  commit's diff and immediately outdated if the line changed since.
* Merge queue not implemented.
