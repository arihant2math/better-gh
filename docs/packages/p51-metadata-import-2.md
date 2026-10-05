Integration: ready
Metadata importer part 2: pull requests, reviews, review comments, wiki, webhooks/protection/rulesets, GitLab source, mannequin reclaim (bgh-import, bgh-pulls `import`, migration 6300); web form, mannequin pages, Playwright 29/29.

# P51 — Metadata importer, part 2: pull requests, reviews, wiki, GitLab source, mannequin reclaim — status

Branch `bgh/p51-metadata-import-2`, on top of P18 (`docs/packages/p18-metadata-import.md`)
and P11. Migration `6300_metadata_import_2.sql` (range 6300–6399).

## What it adds to an import

New steps (run order; `steps[]` in the import JSON):

`git, settings, labels, milestones, issues, pulls, reviews, review_comments, comments, events, releases, wiki, hooks, branch_protection, rulesets, teams, finish`

| Step | GitHub / GHES |
|---|---|
| `pulls` | `GET /pulls?state=all&sort=created&direction=asc` (paged). **Original numbers** (shared sequence with issues, P18 reserved them), title/body, author, assignees, labels, milestone, lock, `created_at`/`updated_at`/`closed_at`, state, **merged** + `merged_at` + `merge_commit_sha` + `merged_by` (single PR GET for merged ones), draft, `maintainer_can_modify`, requested reviewers (users; teams by slug in the target org), reactions (count stashed from the issues list, then `/issues/{n}/reactions`). Refs: one fetch of `+refs/pull/*/head:refs/pull/*/head`; per PR, a head missing locally is fetched by `refs/pull/{n}/head`, then by SHA (works for deleted branches and forks). Open PRs get a missing base branch (and same-repo head branch) **recreated** at the source SHA; closed ones keep only `refs/pull/{n}/head`. Fork PRs: `head_repo_id = NULL` (like a deleted fork). Diff stats/merge base computed when both commits are present; open PRs get a mergeability refresh (no CODEOWNERS requests). |
| `reviews` | per imported PR `GET /pulls/{n}/reviews`: state (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED`; `PENDING` skipped), body, author, `commit_id`, `submitted_at`. |
| `review_comments` | `GET /pulls/comments?sort=created&direction=asc` (repo-wide, paged): review id, `in_reply_to_id` (threads), path, `line`/`original_line`, `side`, `start_line`/`original_start_line`/`start_side` (multi-line), `position`/`original_position`, `commit_id`/`original_commit_id`, `diff_hunk`, `subject_type` (`file` comments), timestamps, reactions. **Outdated**: GitHub's `position: null` is kept (the comment stays on `original_commit_id`). |
| `comments` | now also PR conversation comments (P18 skipped them). |
| `events` | now also PR events: `merged` (commit = merge commit), `head_ref_deleted`/`head_ref_restored` (`ref` from the PR), `ready_for_review`, `convert_to_draft`, `review_requested`/`review_request_removed` (user or team), `review_dismissed` (review id mapped). |
| `wiki` | fetches `{clone_url minus .git}.wiki.git` (all branches) into the wiki repository; the source branch is pointed at by `master` if it differs. A source without a wiki is logged and skipped. |
| `hooks` | `GET /hooks`: `web` hooks imported **disabled** (GitHub never returns secrets; set it, then activate). |
| `branch_protection` | `GET /branches?protected=true` + `/branches/{b}/protection`: status checks, review rules, push restrictions (users mapped, teams by slug), enforce admins, linear history, force pushes, deletions, creations, conversation resolution, signatures, lock, fork syncing. |
| `rulesets` | `GET /rulesets?includes_parents=false` + `/rulesets/{id}`: repository branch/tag rulesets (conditions, rules, enforcement); bypass actors (source ids) dropped and logged; `push` target skipped. |

`hooks`/`branch_protection`/`rulesets` need admin on the source: a 403/404
logs "not readable with this token" and the step is skipped, not failed.

Options (`POST /_bgh/metadata-imports` body, `options` in the JSON):
`pulls` (default true; reviews, review comments), `wiki` (true),
`repo_config` (true; the three config steps; GitHub only). Comments and
events run when `issues` **or** `pulls` is on.

New counters in `stats`: `pulls`, `reviews`, `review_comments`, `wiki`,
`hooks`, `branch_protections`, `rulesets`, `mr_offset` (GitLab).

Everything keeps P18's guarantees: each object commits with its
`import_mappings` row (resumable, idempotent, reruns add nothing — tested),
no domain events (no webhooks, notifications, activity, Actions, review
requests), sync actions recorded for every row.

## GitLab source

`POST /_bgh/metadata-imports {"kind": "gitlab", "api_url": "https://gitlab.com/api/v4" | "https://HOST/api/v4", "source_repo": "group[/subgroup…]/project", "token": …}`
(CLI: `bgh import gitlab --repo GROUP/PROJECT --owner OWNER [--api-url …]`).
Token in `PRIVATE-TOKEN` (git: `oauth2:{token}`); `RateLimit-*` headers and
`Retry-After` handled like GitHub's. `imports.kind` = `gitlab`.

| GitLab | here |
|---|---|
| project (description, `topics`/`tag_list`, `issues_enabled`, `wiki_enabled`, visibility) | settings / default visibility; name = project `path` |
| labels (`#rrggbb`), milestones (`iid`, `due_date`) | labels, milestones with their numbers |
| issues (`iid` → number, `opened`/`closed`, author, assignees, labels, milestone, `closed_by`, `discussion_locked`) | issues; **confidential issues are skipped for public targets** (logged) |
| award emoji (`thumbsup`, `thumbsdown`, `laughing`, `tada`, `confused`, `heart`, `rocket`, `eyes`) | reactions (others dropped); fetched when `upvotes + downvotes > 0` |
| issue notes | comments (system notes dropped) |
| merge requests | pull requests numbered **`iid + offset`**, offset = the highest issue iid, fixed at first use (`stats.mr_offset`), because GitLab numbers issues and MRs separately and GitHub shares one sequence (logged `!iid → #n`). State `merged`/`closed`/`opened`/`locked`, `diff_refs` (single MR GET), `merge_commit_sha`/`squash_commit_sha`, `merge_user`, `closed_by`, draft, reviewers → requested reviewers, approvals → `APPROVED` reviews |
| `refs/merge-requests/{iid}/head` | staged under `refs/bgh/import/mr/*` by one glob fetch, then `refs/pull/{n}/head` (fallback by name, then SHA); staging refs removed at `finish` |
| MR discussions | `DiffNote` discussions → a `COMMENTED` review per discussion with the root + replies as a review thread, resolved state kept; non-diff notes → conversation comments |
| `{project}.wiki.git` | wiki |

Diff positions: a `DiffNote` position (`new_line` → RIGHT, else `old_line` →
LEFT) is located with `bgh_pulls::comments::locate` on its own head commit
(diff hunk, original position) and on the imported PR head; it is
**outdated** (no position, stays on its commit) when the line is gone at
the head or reads differently there (the hunks' target lines differ).
GitLab steps not applicable (releases, events, repo config, teams) show as
`skipped`. Reruns skip issues newer than the MR offset (they would collide).

## Mannequin reclaim

| Method + path | Who | What |
|---|---|---|
| `GET /_bgh/orgs/{org}/mannequins` | org owners (members 403, others 404) | mannequins this org's imports created or reclaimed: `id, login, source, source_login, avatar_url, html_url, reclaimed_by, pending_reclaim{id, target, created_at}, created_at`; `Link` paging |
| `GET /_bgh/admin/mannequins` | site admins | every mannequin |
| `POST /_bgh/mannequins/{id}/reclaims {"login"}` | owners of an org whose import created it, site admins (others 404) | 201 reclaim; 422 `errors[].field=login` (unknown, an org, a mannequin), already reclaimed, one already pending |
| `DELETE /_bgh/mannequin-reclaims/{id}` | same | withdraw a pending one → 204 (else 422) |
| `GET /_bgh/user/mannequin-reclaims` | the invitee | pending first, then answered (≤ 50) |
| `POST /_bgh/user/mannequin-reclaims/{id}/accept` / `decline` | the invitee (others 404) | 200 reclaim; 422 when not pending |

Reclaim JSON: `id, status (pending/accepted/declined/cancelled), mannequin, target, invited_by, organization, moved{"table.column": rows}, created_at, updated_at, completed_at`.
The reclaim belongs to the inviter's org (site admins: the creating org).

Accepting rewrites in **one transaction** every single-column foreign key to
`users(id)` found in the catalog (`pg_constraint`), so tables added later are
covered; identifiers come from `regclass`/`quote_ident`, values are bound.
A row that would violate a unique constraint (the target already reacted,
is already an assignee, …) is dropped (row-by-row savepoints only on
conflict). Also rewritten: user ids inside `issue_events.data`
(`assignee_id`, `assigner_id`, `requested_reviewer_id`) and the source
user's `import_mappings` row (later imports attribute to the real account).
Moved issues, comments, reviews, review comments, events and milestones are
re-synced. The mannequin row stays with `mannequin_reclaimed_by` (links
resolve; it owns nothing). Audit: `org.mannequin_reclaim_invite`,
`org.mannequin_reclaim_accept`.

## Web

* Import form (`pages/imports/ImportForm.tsx`): platform GitHub.com / GHES /
  **GitLab** (host, `group/project` or project URL, token hint), new steps
  "Pull requests" (GitLab: "Merge requests"), "Wiki", "Webhooks, branch
  protection and rulesets"; GitHub-only steps hidden for GitLab.
* Import detail: new steps and counters; "Reclaim mannequins" link when an
  import created mannequins.
* **Organization settings → Mannequins** (`/organizations/:org/settings/mannequins`,
  `g n`) and **Site admin → Mannequins** (`/site-admin/mannequins`, `g q`):
  `pages/imports/MannequinList.tsx` (state pills, "Reclaim…" with inline
  login field and server field errors, "Withdraw").
* **Settings → Imported contributions** (`/settings/reclaims`):
  `pages/settings/sections/ReclaimSettings.tsx` (pending with Accept… /
  Decline, a confirm dialog before moving, history with moved counts).
* Dashboard banner (`pages/imports/ReclaimBanner.tsx`, dashboard chunk) for
  pending invitations.
* All new pages are lazy route chunks. Initial JS 144.4 KB gzip (base
  144.2 KB): the +0.2 KB are the three route-table entries themselves.
* Mocks: `src/mock/extra/metadataImports.ts` (new steps, GitLab kind),
  `src/mock/extra/mannequins.ts` (two seeded mannequins; invite the
  viewer's own login to try accepting).
* Playwright: `web/scripts/metadata-import-2-smoke.mjs` against a real
  server (see its header), the server's own REST API as GHES source:
  **29/29** — form (GHES + GitLab variants), import completes with PR /
  review / review-comment counters, imported open PR keeps number/state/head
  and the merged one its merge commit, **the PR page renders the review
  body, the inline comment and its reply**, the unmapped reviewer shows as
  `bob-imported`; org Mannequins page, invalid login on the field,
  invitation pending, invitee's dashboard banner and settings page, accept,
  comment and review now attributed to bob, mannequin shows reclaimed, no
  page errors. P18's smoke script updated (15 steps, new heading).

## Shared-code changes (additive)

* `bgh-git/fetch.rs`: `fetch_refspecs` (explicit refspecs, same locked-down
  remote as `fetch`).
* `bgh-pulls`: new `pub mod import` (`insert_pull`, `insert_requested_reviewer`,
  `insert_review`, `insert_review_comment`, `resolve_thread`,
  `insert_comment_reactions`).
* `bgh-import`: deps on `bgh-pulls`, `bgh-wiki`, `bgh-git`; new modules
  `pulls`, `gitops`, `repo_config`, `gitlab`, `reclaim`; `Ctx` internals
  `pub(crate)`; `paged` split into `paged`/`dispatch`/`per_mapped`;
  `GitHub.gitlab` (auth header); `Users` understands GitLab user objects.
* `bgh-server`: `bgh import gitlab`; shared `ImportArgs`.
* `docs/ARCHITECTURE.md`: crate line, CLI, metadata import bullet.

## Migration `6300_metadata_import_2.sql`

* `imports.kind` check: `github`, `gitlab`.
* `users.mannequin_reclaimed_by`.
* `mannequin_reclaims` (+ unique pending per mannequin, target/org/mannequin indexes).
* `import_mappings_user_local_idx` (user mappings by local id).

## Tests

`cargo test -p bgh-import` — 12 integration + 7 unit tests, all green:

* `pulls::imports_pull_requests_reviews_wiki_and_repo_config` — acceptance:
  generic fake API (`tests/it/fake_api.rs`, route table → fixtures, token
  auth, `Link` paging, ETag) serving `fixtures/github-pulls/` (generated by
  `gen.py`, GitHub REST shapes) over **real commits** of a git source on
  the test server (`tests/it/pr_source.rs`: merged branch, deleted branches
  kept only by hidden refs). Issues 1, 2, 5 and PRs 3 (merged), 4 (closed
  fork PR, branch gone → fetched by SHA), 6 (open draft, deleted head branch
  recreated) keep numbers, state, merge commit/time/merger, heads
  (`refs/pull/N/head`), diff stats; reviews (states, authors, times);
  review comments (line/side/position, thread reply, multi-line, outdated,
  file-level, reactions); requested reviewers; PR comments, reactions and
  events (`merged`, `head_ref_deleted`, `review_requested`; `subscribed`
  dropped); wiki page; webhook disabled; branch protection; ruleset. No
  webhook deliveries but `repository`, no notifications, sync actions
  recorded. Rerun adds nothing.
* `pulls::config_steps_are_skipped_without_admin_on_the_source`.
* `gitlab::imports_a_gitlab_project_with_merge_requests` — `fixtures/gitlab/`
  (REST v4 shapes, `gen.py`): validation (`kind`, path, token), issues with
  iids, award emoji, notes (system dropped), confidential skipped, MRs
  !1–!3 → #4–#6 (merged/open/closed), approvals, reviewers, `refs/pull`
  heads incl. by SHA, staging refs removed, diff discussion → thread at
  position 3 with reply and resolution, outdated note on a rewritten line,
  individual note → comment, wiki, numbering continues at 7, rerun no-op.
* `reclaim::reclaiming_a_mannequin_moves_attribution` — list permissions,
  invite validation, pending state, nothing moves before acceptance, only
  the invitee answers, accept moves reviews/review comments/issues/comments
  (counts in `moved`), mapping re-pointed, re-sync, listed as reclaimed,
  audit.
* `reclaim::declining_and_cancelling_leave_attribution_alone` (+ a site
  admin's reclaim belongs to the creating org).
* P18's tests run with `pulls`/`wiki`/`repo_config` off (their fixtures
  don't serve PRs); `self_import` runs with everything on against this
  server's own API.

## Known gaps / follow-ups

* Review thread **resolution** isn't in GitHub's REST API (GraphQL only):
  GitHub threads import unresolved (GitLab's resolved state is kept).
* GitHub PR `closed_by` isn't in the PR payloads (the `closed` event keeps
  the actor). Commit comments, PR review reactions on reviews themselves,
  check runs/statuses, and `head_ref_force_pushed`/`base_ref_changed`
  events (no before/after in the events API) aren't imported.
* Webhook secrets can't be read from GitHub; hooks arrive disabled.
  Ruleset bypass actors and app-based status check sources are dropped.
* GitLab: releases, system-note events (label/milestone changes), protected
  branches, project hooks, epics and group-level labels/milestones are not
  imported; award emoji on notes aren't fetched. Issues created on GitLab
  after the first import whose iid exceeds the MR offset are skipped on
  rerun (logged).
* Reclaim moves attribution site-wide (mannequins are per source host, not
  per organization); an org owner can only start it for mannequins its own
  imports created, and the invitee must accept. Mentions (`@login`) in
  bodies are not rewritten.
