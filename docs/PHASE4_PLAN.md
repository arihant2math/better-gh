# Better GitHub: next-phase implementation plan

This plan merges the gap audits for 8 domains (repos-git, issues-projects, pulls, accounts-orgs, notify-search-activity, actions-packages-security, admin-ops, web-ux-perf, api-compat) into 86 work packages. Each package is sized for one autonomous worker session of about 1–3 hours, covering backend, web UI and tests. Packages are ordered by priority: P1–P22 are priority 1, P23–P77 priority 2, P78–P86 priority 3. They are grouped into parallel waves so that packages in the same wave touch mostly separate crates and files.

## 0. Conventions for every worker (read first)

- **Process.** Follow `docs/WORKER_GUIDE.md`.
  - Work on branch `bgh/p<NN>-<slug>`, cut from `origin/claude/sleepy-cray-9jj0t3`. Merge the integration branch at least hourly. No rebase or force-push.
  - Keep `docs/packages/<slug>.md` up to date: status, endpoints, tables, shared-code changes and known gaps.
- **Migrations.** *Superseded (#220): these ranges are historical; new migrations follow ARCHITECTURE.md "Migrations" (highest on `main` + 10). Out-of-order landing is not safe: it changes the order between fresh and upgraded installs.*
  - Existing files use 0001–1299. Ranges 1300 and up are free.
  - Package **Pn owns `migrations/{1200+100·n}`–`{1299+100·n}`**. For example, P1 owns 1300–1399, P22 owns 3500–3599 and P86 owns 9800–9899.
  - Use only your own range, and never edit an existing migration. Packages marked "likely unused" probably need no schema.
  - sqlx (`bgh-core/src/db.rs` MIGRATOR) applies any migration not yet applied, so landing one out of numeric order across waves is safe.
- **Shared hotspots: additive edits only. These owners do the larger edits in their wave:**

  | File / area | Owner (wave) | Rule for everyone else |
  |---|---|---|
  | `bgh-core/src/events.rs` | P9 (W1): delivery mechanics | Add new variants or fields with `#[serde(default)]` only |
  | `bgh-core/src/registry.rs` | P9 (W1) | Periodic work is a `reg.service(..)` loop with a pg advisory lock, so this file needs no changes |
  | `bgh-core/src/perms.rs` | W1: P7 (visibility), P8 (job-token caps). W2: P17. W3: P47. W4: P48 | Add new functions, don't restructure |
  | `bgh-core/src/auth.rs` | P14 (W1, basic auth), P17 (W2, JWT/installation), P47 (W3) | Additive |
  | `bgh-git/src/storage.rs` `REPO_CONFIG` | P2 (W1) | Nobody else edits it |
  | `bgh-notify/src/payloads/mod.rs` | P10 (W1) | Later packages add one match arm plus a builder in a new `payloads/<feature>.rs` |
  | `bgh-actions/src/trigger.rs` | P8 (W1, guard only), P26 (W2), P20 (W3) | Additive arms |
  | `web/src/app/routes.ts` | anyone | Additive entries only, kept above the `/:owner/:repo/:tab` catch-all |
  | `web/src/pages/pulls/MergeBox.tsx` | P39 (W3), P40 (W4), P74 (W5) | Nobody else |
  | `web/src/pages/issues/Timeline.tsx` | P41/P42 (W3, separate regions), P54 (W4), P74 (W5) | Nobody else |
  | `bgh-graphql` | Feature packages add new files under `model/` and `mutation/`. P44/P45 own existing model files | Keep `query.rs` `node()` edits to one arm each |
- **Baseline acceptance (implicit in every package):**
  - Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p <crates>`, then the full `cargo test --workspace` before finishing.
  - In `web/`, run `npm run typecheck && npm run lint && npm test && npm run build` (the size budget must hold).
  - `scripts/gh-compat.sh` and `scripts/api-smoke.sh` stay green.
  - Every new REST endpoint has an integration test that checks its JSON shape against docs.github.com: status codes, the error shape, and pagination `Link` headers.
  - Every endpoint the web app uses is also added to `web/src/mock/` so `npm run dev:mock` keeps working.
  - Every new webhook-able action is listed in P10's coverage test.

## 1. Wave schedule (parallel batches)

| Wave | Packages | Notes |
|---|---|---|
| **W1** | P1 P2 P3 P4 P5 P6 P7 P8 P9 P10 P11 P12 P13 P14 P15 P16 | Critical bugs, security, and the adoption blockers |
| **W2** | P17 P18 P19 P21 P22 P23 P24 P26 P31 P32 P33 P34 P35 P36 | P18 needs P11. P24 is built against GitHub's REST contract in parallel with P23 |
| **W3** | P20 P25 P27 P29 P37 P38 P39 P41 P42 P44 P46 P47 P50 P51 P61 P65 | P20 needs P19. P39 needs P23 and P26. P44 needs P19 and P23. P46/P47 need P17 |
| **W4** | P28 P30 P40 P43 P45 P48 P49 P52 P53 P54 P55 P56 P58 P60 P62 P63 P66 | P45 needs P38, P41 and P42. P49 needs P14. P66 needs P65 |
| **W5** | P57 P59 P64 P67 P68 P69 P70 P71 P72 P73 P74 P75 P76 P78 P79 | Polish packages run last in their area |
| **W6** | P77, P80–P86 | Docs consolidation, then priority-3 features |

**Dropped as out of scope:** explore/trending, Marketplace, Sponsors, Copilot-like features, billing.

**Deferred (not packaged):**
- i18n infrastructure
- NuGet, RubyGems and Cargo registries (after P81)
- Artifact attestations (after P28)
- Repository custom properties
- GHES cluster mode and geo-replication (P77 gives HA guidance only)
- S/MIME signature verification
- Discussion polls (stretch goal in P56)

---

## 2. Priority 1 packages

### P1 — Fork-safe git maintenance and scheduled housekeeping
- **Priority** 1 · **Wave** W1 · **Migrations** 1300–1399
- **Touches:** `bgh-git` (new `maintenance.rs`, `archive.rs`), `bgh-admin/src/maintenance.rs`, `bgh-repos` (`maintenance.rs`, `forks.rs`, repo delete path in `repos.rs`), web site-admin maintenance page
- **Scope**
  - Add `bgh_git::maintenance`, which is aware of fork networks:
    - Detect dependents: repositories whose `objects/info/alternates` points at this repo. Use the DB fork relationships, with a filesystem check as a fallback.
    - Parent with dependents: `git repack -a -d --keep-unreachable` only. Never prune.
    - Repo without dependents: `git gc --prune=<grace>`, with a default grace of 2 weeks set by a site setting.
    - Fork (has alternates): `git repack -a -d -l`.
    - Remove every `--prune=now` from the codebase.
  - Rewrite the admin `gc` and `repack` actions in `bgh-admin/src/maintenance.rs`, including `schedule_all`, to call these functions.
    - A "prune unreachable now" action is allowed only on repos without dependents. It needs an explicit `force` flag and is audited.
  - Deleting a parent, or detaching a fork from its network:
    - Before deleting the parent's storage, dissociate each fork: run `git repack -a -d` without `-l` inside the fork, remove `objects/info/alternates`, then verify with `git fsck --connectivity-only`.
    - Do the same when a fork is detached ("leave fork network" admin action).
  - Add a scheduled maintenance service, `reg.service("repos.maintenance")`:
    - Use a pg advisory lock to elect a single leader.
    - Pick due repos by pushes since the last run, loose-object count and pack count (`git count-objects -v`).
    - Run `git commit-graph write --reachable --split --changed-paths` on every repo.
    - Run a geometric repack (`--geometric=2`). Write bitmaps and midx only for repos without alternates.
    - Run a full repack on a longer cadence.
    - Use a per-repo lock, and skip a repo while it has an active receive-pack.
  - Add a `repo_maintenance` table: `repo_id`, `last_run_at`, `last_full_at`, `status`, `error`, `pack_count`, `loose_count`.
  - Show per-repo status and the schedule settings in admin.
  - Prune the archive cache on a schedule by calling `archive::prune_cache`, with max age and max size settings.
  - Correct the backup text in `docs/SELF_HOSTING.md`: objects can be removed after the grace period.
- **Acceptance**
  - Fork-corruption test:
    - Create a parent and a fork. Push branch X to the parent; the fork references X's commit through alternates only.
    - The parent deletes or force-pushes X. Run admin gc, admin repack and the scheduled job on the parent.
    - The fork still clones, `git fsck --connectivity-only` is clean, and the browse and PR APIs work.
  - Deleting a parent that has forks leaves the forks cloneable with no alternates file.
  - After a scheduled run, `objects/info/commit-graphs` exists. A bitmap exists only on a repo without forks. Old archives are pruned.
  - A push that runs concurrently with the repack succeeds, and refs stay valid.
  - `grep -rn 'prune=now' crates` returns nothing (enforce with a unit test).

### P2 — Push and ref hardening: hidden refs, fsck, size limits, quotas
- **Priority** 1 · **Wave** W1 · **Migrations** 1400–1499 (likely unused)
- **Touches:** `bgh-git` (`storage.rs` `REPO_CONFIG` owner, `smart_http.rs` pre-receive `PushPolicy`), `bgh-repos` (`git_http.rs`, `ssh/exec.rs`, `refs.rs`/`gitdb.rs`, `lfs/`), `bgh-core/src/settings.rs` (`check_push_quota` and a `git` settings section)
- **Scope**
  - Hidden refs:
    - Add `receive.hideRefs = refs/pull/` and `refs/bgh/` to `REPO_CONFIG`.
    - Also reject those namespaces in `authorize_push` for both HTTP and SSH, as defense in depth.
    - `POST`/`PATCH`/`DELETE /git/refs` on `refs/pull/*` returns 422 in GitHub's shape.
    - Internal writers (`bgh-pulls` `mirror_head` and the merge refs) keep working.
  - fsck:
    - Set `receive.fsckObjects = true`, controlled by site setting `git.fsck_on_push` (default true).
    - Downgrade common historical warnings with `receive.fsck.<id>=ignore`: `zeroPaddedFilemode`, `badTimezone`, `missingSpaceBeforeDate`.
    - Keep `.gitmodules` and symlink checks fatal.
  - Upgrade existing repos with a one-shot, idempotent job that rewrites every repo's config. Stamp it with `bgh.configVersion`, and run it at startup when the version is behind.
  - Size limits:
    - Add settings `git.max_object_size_mb` (default 100) and `git.warn_object_size_mb` (default 50).
    - In the quarantine pre-receive, enumerate new objects (`rev-list --objects` plus `cat-file --batch-check`) and reject oversize blobs. Use the exact GH001 text: `GH001: Large files detected. You may want to try Git Large File Storage`.
    - Send a warning over sideband for blobs above the warn size.
    - Set `receive.maxInputSize` from `git.max_push_size_mb`.
  - Quotas:
    - `check_push_quota` includes `lfs_size`.
    - Pre-check the incoming quarantine size against the remaining quota, so a single push can't overshoot.
    - An LFS batch upload over quota returns the spec error (HTTP 507 with an LFS JSON error).
- **Acceptance**
  - Pushing to `refs/pull/1/head` is rejected over both HTTP and SSH with `deny updating a hidden ref`. `POST /git/refs` with `refs/pull/x` returns 422.
  - PR head and merge refs still update. A `git push --mirror` of a clone that contains `refs/pull/*` rejects only those refs.
  - Pushing a malicious `.gitmodules` (`url = --upload-pack=…` or path traversal) is rejected.
  - A 101 MB blob is rejected with the GH001 text. A 60 MB blob is accepted with a warning.
  - An over-quota push is rejected before refs change. An over-quota LFS upload is rejected.
  - Existing repos have the new config after the upgrade job.

### P3 — PR merge governance: rulesets on merge, fork-status spoofing, review options
- **Priority** 1 · **Wave** W1 · **Migrations** 1500–1599 (likely unused)
- **Touches:** `bgh-pulls` (`protection.rs`, `mergeability.rs`, `automerge.rs`, `merge.rs` pre-checks, `reviews.rs` dismiss), `bgh-repos/src/protection.rs` (add a `pub fn` ruleset loader/evaluator)
- **Scope**
  - Required checks count only `commit_statuses` and `check_runs` whose `repo_id` is the base repo. Remove `head_repo_id` from `check_outcomes`.
  - Honor `checks[].app_id` (the expected source) for check runs. `-1` or absent means any source.
  - Build one effective rule set per (repo, base branch):
    - Union classic protection with every **active** matching ruleset, taking the most restrictive value for `required_approving_review_count`, `require_code_owner_review`, `required_review_thread_resolution`, `require_last_push_approval`, `dismiss_stale_reviews_on_push`, `required_status_checks` (with `strict_required_status_checks_policy`) and `required_linear_history`.
    - Leave a hook for `required_signatures` (P25), `merge_queue` (P39) and `required_deployments` (P20).
    - The org-ruleset loader plugs in through P23.
  - Implement the remaining rules:
    - `require_last_push_approval`: the approver is not the last pusher, using the recorded pusher of the latest synchronize.
    - Thread-resolution requirement.
    - `dismissal_restrictions` for dismissing reviews.
    - `bypass_pull_request_allowances`.
    - Classic `restrictions` (users, teams or apps allowed to push) also gate merge.
    - Ruleset bypass actors (`always` / `pull_request`) allow the admin "merge without waiting for requirements" path.
  - The merge API on a violation returns 405 `Repository rule violations found` with one line per failing rule, in GitHub's style.
  - `mergeable_state` becomes `blocked`.
  - The `/_bgh` merge-box status returns each failing requirement with its source (classic rule or ruleset name). Render it as a list in `MergeBox` with a minimal text-only change; P39 owns the larger edits.
  - Auto-merge uses the same evaluator.
- **Acceptance**
  - A ruleset requiring 2 approvals and the check `ci` blocks `PUT /merge` and auto-merge until both are satisfied.
  - Fork-spoof test: base CI posts `ci=failure`, and the fork owner posts `ci=success` on the fork repo for the same sha. The PR stays blocked.
  - An `app_id` mismatch doesn't satisfy the check.
  - Last-push approval test.
  - A dismissal by a user outside `dismissal_restrictions` returns 403.
  - `gh pr merge` shows GitHub's error text.

### P4 — Closing keywords, linked issues/PRs and the Development section
- **Priority** 1 · **Wave** W1 · **Migrations** 1600–1699
- **Touches:** `bgh-issues` (`refs.rs`, new `links.rs`, events), `bgh-core/src/sync/shapes.rs` (new link model, additive), `bgh-graphql/src/loaders.rs` (`closingIssuesReferences`, `closedByPullRequestsReferences`), web `pages/issues/IssueSidebar.tsx`, the PR sidebar, `IssueList` row indicator, and Timeline event renderers for the new events only
- **Scope**
  - Add table `issue_pr_links(issue_id, pull_id, source enum(keyword|manual), created_by, created_at)`, unique on `(issue_id, pull_id)`.
  - Extend `closing_refs`:
    - Keywords: close(s|d), fix(es|ed), resolve(s|d).
    - Reference forms: `#N`, `owner/repo#N`, full issue URLs on this host.
    - Cross-repo links only when the PR author has triage on the target.
  - Reconcile keyword links when a PR is opened or its body is edited. Emit `connected`/`disconnected` issue events (timeline and webhooks).
  - Add a `/_bgh` manual link/unlink API: write access on both.
  - Listen for `PullRequestMerged`. When base equals the default branch, close every linked issue:
    - Set `state_reason=completed`.
    - Write a `closed` event with `commit_id` = merge sha and the PR as source ("closed this as completed in #N").
    - Emit `IssueClosed` so webhooks and Actions fire.
  - GraphQL `PullRequest.closingIssuesReferences` and `Issue.closedByPullRequestsReferences` read from the table instead of parsing the body.
  - Web:
    - Issue sidebar "Development" section: linked PRs with state icons, linked branches, and "Link a pull request" with a picker.
    - PR sidebar: "Successfully merging this pull request may close these issues" plus a link picker.
    - Issue list rows show a linked-PR icon and count.
    - Timeline renders `connected`, `disconnected` and closed-by-PR.
- **Acceptance**
  - A PR whose body has `Fixes #12, closes other/repo#3, resolves https://host/o/r/issues/4`, merged into the default branch, closes all three issues with the right events and webhooks.
  - Merged into a non-default branch, it closes none.
  - Removing a keyword produces `disconnected`.
  - A manually linked issue closes on merge.
  - `gh pr view --json closingIssuesReferences` returns the links.
  - UI smoke: the sidebar shows the link and updates live through sync.

### P5 — Invitation acceptance UI and org membership self-service
- **Priority** 1 · **Wave** W1 · **Migrations** 1700–1799 (likely unused)
- **Touches:** web (new `pages/invitations/`, dashboard banner, `pages/settings` organizations section), `routes.ts`, `bgh-accounts/src/orgs.rs`, `bgh-repos/src/collaborators.rs` (`html_url` check)
- **Scope**
  - New routes, placed above the catch-all:
    - `/orgs/:org/invitation`: shows org, role and inviter, with Accept (`PATCH /user/memberships/orgs/{org}` `{state:"active"}`) and Decline. Add a `/_bgh` decline endpoint if none exists.
    - `/:owner/:repo/invitations`: the viewer's pending repo invitation, with Accept (`PATCH /user/repository_invitations/{id}`) and Decline (`DELETE`).
  - Dashboard banner lists pending invitations from `GET /user/repository_invitations` and `GET /user/memberships/orgs?state=pending`.
  - A signed-out user opening an invitation link goes through sign-in or sign-up (honoring the org-invite sign-up policy) with `return_to` back to the invitation page.
  - New `/settings/organizations` page:
    - Lists memberships.
    - "Leave" (`DELETE /orgs/{org}/memberships/{me}`), refusing to remove the last owner with a GitHub-shaped 403.
    - Public-membership toggle (`PUT`/`DELETE /orgs/{org}/public_members/{u}`).
  - `billing_manager` invitations return 422 (`role` must be `admin`, `direct_member` or `reinstate`). Fix `invitation_member_role`.
  - Verify the email templates link to the routed pages.
- **Acceptance**
  - E2E via API plus UI smoke: user A invites B to an org and a repo. B sees the banner, accepts both, and gains access. Decline removes the invitation.
  - `billing_manager` returns 422.
  - Leaving an org removes access, and the last owner can't leave.
  - Vitest coverage for both pages.

### P6 — Attachments: image and file uploads in comments, PRs, releases and wiki
- **Priority** 1 · **Wave** W1 · **Migrations** 1800–1899
- **Touches:** new crate `bgh-uploads` (registered in `bgh-server`), `web/src/components/editor/MarkdownEditor.tsx`, wiki editor, `web/src/ui/markdown` (video embed), `bgh-core` quota helper
- **Scope**
  - Content-addressed storage under `{data_dir}/files/attachments/`.
  - Table `attachments(uuid, uploader_id, owner_id, repo_id NULL, name, content_type, size, sha256, created_at)`.
  - `POST /_bgh/uploads?repository_id=&owner_id=` (multipart) returns `{id, href, markdown}`.
  - Serving:
    - Serve at GitHub-like paths: `GET /user-attachments/assets/{uuid}` and `/user-attachments/files/{id}/{name}`.
    - If the attachment belongs to a private repo, require read access; otherwise return 404.
    - Headers: `X-Content-Type-Options: nosniff` and `Content-Security-Policy: default-src 'none'; sandbox`.
    - SVG and HTML are served as `attachment`. Images and video are served inline. Support Range for video.
  - Limits follow GitHub: images 10 MB, video 100 MB, other files 25 MB, with GitHub's allowed extensions list. Count uploads against the owner's quota. Delete a repo's attachments when the repo is deleted.
  - Editor:
    - Paste, drop and an "Attach files" button.
    - Insert placeholder text `![Uploading name…]()`, then replace it with `![name](url)` or `[name](url)`.
    - Show progress and error toasts.
    - Works in issue and PR comments, review comments, release notes and the wiki editor.
  - The markdown renderer turns bare attachment video URLs into `<video controls>`.
- **Acceptance**
  - Integration tests:
    - Upload and download work.
    - A private-repo attachment returns 404 for an outsider.
    - Oversize and disallowed types are rejected with 422.
    - CSP and nosniff headers are present, and SVG gets `Content-Disposition: attachment`.
  - Vitest: the placeholder is replaced, and a failure removes it.
  - Manual: paste a screenshot into an issue comment and it renders after reload.

### P7 — Access policy: internal visibility, private mode, allowed visibilities
- **Priority** 1 · **Wave** W1 · **Migrations** 1900–1999 (likely unused)
- **Touches:** `bgh-core` (`perms.rs` `public_floor` and readable-repos SQL, `models/db.rs`, `settings.rs` new `privacy` section), `bgh-server` middleware, `bgh-accounts` (`users.rs`/`orgs.rs` list endpoints, `members_can_create_internal_repositories`), `bgh-repos` (create, PATCH, transfer and fork visibility checks; anonymous SSH and HTTP), `bgh-search` filters, web (`RepoLayout` badge, `NewRepoPage`, admin settings section)
- **Scope**
  - `internal` visibility: Read for every authenticated, non-suspended user (GHES semantics), never for anonymous users. Apply this in `public_floor`, the readable-repos SQL, and the repo, code, issue and commit search filters.
  - Repo JSON shows `private: true, visibility: "internal"`. The badge reads "Internal". Forks of internal repos stay internal, within an org.
  - Bind `members_can_create_internal_repositories` in `PATCH /orgs` and enforce it.
  - Private mode: `privacy.private_mode` (default false).
    - Every anonymous request is refused.
    - API: 401 `Requires authentication`.
    - Git HTTP: 401 with `WWW-Authenticate`.
    - Web: redirect to `/login`.
    - Raw files, archives, LFS, avatars and the sync WebSocket require auth.
    - Exempt: login, sign-up, password reset, OAuth/SSO/device flow, static assets, `/healthz` and `/api/v3/meta`.
  - `GET /users` and `GET /organizations` require auth in private mode, or whenever `privacy.allow_anonymous_directory=false`.
  - `privacy.allowed_visibilities` ⊆ {public, internal, private}, enforced on create, PATCH, transfer, fork and template generate (422 with a GitHub-style message). `default_visibility` must be in the allowed set.
  - Admin settings UI "Privacy" section. `public_info` exposes private mode to the sign-in page.
- **Acceptance**
  - An internal repo is readable by an authenticated non-member, not writable, absent for anonymous users, and present in that user's search results.
  - In private mode:
    - Anonymous `GET /api/v3/repos/o/r` returns 401.
    - Anonymous `git clone` returns 401.
    - The web redirects to login.
    - With a token, `scripts/gh-compat.sh` passes.
  - Creating a public repo when it isn't allowed returns 422.

### P8 — Actions token and runner security
- **Priority** 1 · **Wave** W1 · **Migrations** 2000–2099
- **Touches:** `bgh-actions` (`server.rs` token minting, `runner/mod.rs` executor selection, `runner/process.rs` env, `trigger.rs` loop guard only), `bgh-core` (new `token_permissions.rs` route-category table, job-token branch in `perms.rs`, an additive `actor_kind`/`via_actions_token` field on events, bot user seed), `bgh-repos` (workflow-file check in receive-pack and the contents API), `Dockerfile`/`docker-compose.yml`, `docs/SELF_HOSTING.md` Actions warning
- **Scope**
  - Runner isolation:
    - The shell executor spawns steps with `env_clear()` plus an allowlist (PATH, HOME, LANG, TMPDIR, the job env).
    - `BGH_ACTIONS_EXECUTOR=auto` never falls back to shell. If docker is unavailable, the built-in runner logs a loud warning and takes no jobs.
    - Shell requires an explicit `BGH_ACTIONS_EXECUTOR=shell`, with a startup warning that it is for trusted single-tenant installs only.
    - The work dir lives outside `data_dir`.
  - GitHub-style `permissions:`:
    - Parse workflow-level and job-level `permissions:` (`read-all`, `write-all`, `{}`, per category).
    - Site default `actions.default_workflow_permissions` = `read` (GitHub's restricted default); P30 adds repo and org overrides.
    - Store the permission map on the job token (JSON column on `access_tokens`).
    - Enforce it through a route-category table in `bgh-core`: (method, path pattern) → category (`contents`, `issues`, `pull_requests`, `statuses`, `checks`, `actions`, `deployments`, `packages`, `pages`, `security_events`, `metadata`), read or write.
    - The table is applied in middleware for job tokens. P17 reuses it for installation tokens.
    - Fork `pull_request` runs get read-only tokens and no secrets.
  - `workflow` scope:
    - Receive-pack (HTTP and SSH) and contents-API writes that touch `.github/workflows/**` require the `workflow` scope for PAT and OAuth tokens.
    - Use GitHub's message: `refusing to allow a Personal Access Token to create or update workflow `<path>` without `workflow` scope`.
    - Job tokens are always refused. Web session edits are allowed.
  - Loop guard:
    - Events caused by a job token carry `via_actions_token: true`.
    - `trigger.rs` ignores them, except `workflow_dispatch` and `repository_dispatch`.
  - Attribution:
    - Seed a `github-actions[bot]` Bot user. Job-token writes (comments, labels, pushes) are attributed to the bot.
    - The audit log records the triggering actor.
- **Acceptance**
  - `actions-e2e.sh`: a step running `env` shows no `DATABASE_URL`, `REDIS_URL`, `BGH_*` or SMTP variables.
  - With `permissions: {contents: read}`, a push with `GITHUB_TOKEN` returns 403. With `issues: write`, a comment succeeds and is authored by `github-actions[bot]`.
  - A PAT without the `workflow` scope pushing a workflow file is rejected over HTTP, SSH and the contents API.
  - A workflow that commits on push does not re-trigger itself.
  - The default token can't push.

### P9 — Durable event delivery and graceful shutdown
- **Priority** 1 · **Wave** W1 · **Migrations** 2100–2199
- **Touches:** `bgh-core/src/events.rs` and `registry.rs` (owner), `bgh-server/src/main.rs`, idempotency keys in `bgh-notify`
- **Scope**
  - Transactional outbox:
    - `emit` writes to `event_outbox(id bigserial, kind, payload jsonb, created_at)` in the caller's transaction (keep the current API).
    - After commit, wake listeners in-process and with `pg_notify`, so multiple processes work.
  - `on_event` listeners become durable consumers:
    - `event_listener_cursors(listener, last_id)`.
    - Batch reads; delivery is at-least-once.
    - One consumer per listener across processes (advisory lock or lease).
    - No drops on lag.
    - Keep the in-memory broadcast only for ephemeral subscribers (sync WebSocket).
  - Make handlers idempotent. Add unique keys: webhook deliveries `(hook_id, event_id)`, notifications `(user, thread, event_id)`, activity `(event_id)`.
  - Prune outbox rows that all cursors have passed and that are older than N days.
  - Graceful shutdown:
    1. Stop accepting connections.
    2. Await in-flight requests.
    3. Then signal the background token.
    4. Listeners finish the current batch.
    5. Exit.
    Use separate cancellation tokens for HTTP and background work.
  - Expose a consumer-lag function for P61.
- **Acceptance**
  - Restart test: events emitted while a listener is stopped are delivered after it restarts.
  - Burst test: 10k events, no losses.
  - Two registries on the same DB don't double-deliver.
  - SIGTERM test: a request that emits an event during shutdown still produces its webhook delivery.
  - Duplicate redelivery doesn't create duplicate notifications.

### P10 — Webhook and event wiring fixes
- **Priority** 1 · **Wave** W1 · **Migrations** 2200–2299 (likely unused)
- **Touches:** `bgh-notify` (`payloads/mod.rs` owner, new builders, `fanout.rs` CI notifications), emit points in `bgh-repos` (`settings.rs`, `collaborators.rs`, `keys.rs`, `rulesets.rs`, `protection_api.rs`), `bgh-releases/src/releases.rs`, `bgh-wiki` (emit), `bgh-accounts` (only if a payload needs data)
- **Scope.** Map or emit each of the following:
  - `star` created/deleted and `watch` started, from `RepositoryStarred`.
  - `repository` archived/unarchived/publicized/privatized/transferred, plus `edited` with a real `changes` object. Emit `RepositoryArchived/Unarchived/Publicized/Privatized` in `settings.rs`, which also fixes the activity `PublicEvent`.
  - `member` added/edited (`changes.permission`)/removed.
  - `release` edited (`changes.name`, `body`, `tag_name`), from `ReleaseUpdated`. Also `released`, `prereleased` and `unpublished` where state changes.
  - Checks API: `check_run` created/completed/rerequested/requested_action and `check_suite` requested/completed/rerequested.
  - `ci_activity` notifications for failed external check suites.
  - Org events: `team` (created/edited/deleted/added_to_repository/removed_from_repository), `membership` added/removed, `team_add`, and `organization` member_removed/member_invited. Global hooks receive `team` and `membership`.
  - `issues` pinned/unpinned/transferred.
  - `pull_request` auto_merge_enabled/disabled, `pull_request_review` edited, `pull_request_review_thread` resolved/unresolved.
  - `sub_issues` (sub_issue_added/removed, parent_issue_added/removed).
  - `gollum`: emit from `bgh-wiki` on page create, edit and delete.
  - `deploy_key` created/deleted.
  - `branch_protection_rule` created/edited/deleted and `repository_ruleset` created/edited/deleted.
  - `meta` deleted (hook removed).
  - Payload fidelity:
    - `issue_comment` and `pull_request_review_comment` edited include `changes.body.from`.
    - Deleted-comment payloads include the full object (snapshot taken before the delete).
  - Coverage test: for every event name the hook validator accepts, assert there is an emitter, or that the event is in an explicit "not producible yet" list (installation, dependabot, …).
- **Acceptance**
  - One test per bullet: perform the action, then assert a `webhook_deliveries` row with the right `X-GitHub-Event`, `action` and the required top-level keys from docs.github.com.
  - The coverage test passes.
  - A global hook subscribed to `team` receives team creation.

### P11 — Import repository from URL and pull mirrors
- **Priority** 1 · **Wave** W1 · **Migrations** 2300–2399
- **Touches:** `bgh-repos` (new `import.rs`, `mirrors.rs`, receive guard for mirrors), `bgh-git` (new `fetch.rs`: credentialed fetch, timeouts, LFS fetch), `bgh-core` (secret encryption helper; reuse the `bgh-actions` crypto pattern additively), SSRF guard (reuse `bgh-notify` `ssrf` logic via `bgh-core`), web `/new/import` page, repo settings "Mirror" section, repo header line
- **Scope**
  - `POST /_bgh/imports {source_url, username?, password_or_token?, owner, name, visibility, mirror, include_lfs}`:
    - Creates the repo in `importing` state and runs a background job to fetch all branches and tags (`+refs/heads/*`, `+refs/tags/*`), plus LFS objects when requested.
    - Progress (phase, objects, bytes) is available by polling and through sync. Cancel and retry are supported.
    - Credentials are encrypted at rest and never returned.
    - The SSRF guard blocks private addresses unless allow-listed (the same list as webhooks).
  - `/new/import` becomes a real page (it is currently an alias of `NewRepoPage`): source URL, credentials, owner/name, visibility and "mirror" checkbox, then a progress screen.
  - Pull mirrors:
    - Table `repo_mirrors(repo_id, url, enc_credentials, interval_minutes, enabled, last_sync_at, next_sync_at, last_status, last_error)`.
    - A scheduled service fetches due mirrors with `--prune` and force-updates refs. It emits `PushEvent` with origin `mirror`: search index and activity run, Actions skips.
    - Pushes to a mirror are refused with `This repository is a mirror and is read-only`.
    - `mirror_url` appears in REST JSON. GraphQL `isMirror`/`mirrorUrl` are populated.
    - `POST /_bgh/repos/{o}/{r}/mirror/sync` triggers a sync now.
    - Settings section: interval, credentials, sync now, last error, and "convert to regular repository".
  - Repo header shows "mirrored from <url>".
  - Admin lists mirrors with failures.
- **Acceptance**
  - Integration test imports from a repo served by the test server itself (allow-listed), including tags and an LFS object. Clone works and refs match.
  - A new upstream commit appears after a mirror sync.
  - Pushing to the mirror is rejected. `mirror_url` is set.
  - Credentials are absent from every API response and the audit log.
  - UI smoke covers the import flow and the progress screen.

### P12 — Repo header and navigation (Fork, Watch, Sync fork, template, lists, rename redirects, real 404s)
- **Priority** 1 · **Wave** W1 · **Migrations** 2400–2499 (unused)
- **Touches:** `web/src/pages/repo` (`RepoLayout.tsx`, `RepoPlaceholderPage.tsx`), new list pages, `routes.ts`, `web/src/api/endpoints.ts`
- **Scope**
  - Fork button opens a dialog:
    - Owner select limited to accounts where the user can create repos.
    - Name, description, "copy the default branch only".
    - Submits `POST /forks` and navigates to the fork, showing a "Forking…" state while the 202 is in progress.
  - Watch button opens the existing `WatchDialog` (`ui.openWatch`). The label reflects the state (Watch / Unwatch / Custom).
  - The star, watch and fork counts link to new paginated pages `/:o/:r/stargazers`, `/watchers` and `/forks`.
  - Lines under the repo name: "forked from X" and "generated from X".
  - Sync fork: a dropdown with ahead/behind against upstream and "Update branch" (`POST /merge-upstream`), showing the conflict message on 409.
  - "Use this template" button on template repos, linking to `/new?template_owner=&template_name=`.
  - Rename and transfer redirect: when `getRepository` resolves to a different `full_name`, `router.replace` to the canonical owner/name, keeping subpath, query and hash. This fixes the endless spinner.
  - Hide the Issues and Projects tabs when `has_issues`/`has_projects` is false.
  - Unknown repo sub-paths render a proper 404. Keep the placeholder only for `security` and `pulse` until P31/P66.
  - Routes for `html_url`s the backend emits:
    - `/orgs/:org/teams/:slug` → team page (reuse the org-settings team view, read-only for members).
    - `/:o/:r/runs/:id` → resolve the check run to its Actions job, or to the external `details_url`.
    - `/:o/:r/labels/:name` → issues filtered by label.
    - `/:o/:r/search` → code search scoped to the repo.
    - `/orgs/:org/people` and `/orgs/:org/repositories` → org profile tabs.
- **Acceptance**
  - Vitest covers the redirect logic and the tab visibility.
  - Smoke:
    - Rename a repo, open the old URL, and land on the new URL.
    - Create a fork from the UI.
    - Open the watch dialog from the header.
    - Sync a fork that is behind.
    - `/o/r/doesnotexist` shows a 404.
    - Team `html_url` resolves.

### P13 — Projects v2 public API (GraphQL and REST)
- **Priority** 1 · **Wave** W1 · **Migrations** 2500–2599 (likely unused)
- **Touches:** `bgh-graphql` (new `model/project.rs` and `mutation/projects.rs`; replace stubs in `model/misc.rs`, `repo.rs`, `actor.rs` and `issue.rs`; `create_issue` `projectV2Ids`), `bgh-projects` (REST routes over the existing service layer)
- **Scope**
  - GraphQL types:
    - `ProjectV2`: `id`, `number`, `title`, `shortDescription`, `readme`, `public`, `closed`, `url`, `owner`, `creator`, `createdAt`/`updatedAt`, `fields`, `items`, `views`, `viewerCanUpdate`.
    - Field unions: `ProjectV2Field`, `ProjectV2SingleSelectField` (options), `ProjectV2IterationField` (configuration.iterations, completedIterations).
    - `ProjectV2Item` with `type`, `content` (Issue | PullRequest | DraftIssue), `fieldValues` (Text/Number/Date/SingleSelect/Iteration/Labels/Milestone/Repository/User/PullRequest/Reviewer values), `fieldValueByName` and `isArchived`.
  - GraphQL query entry points: `User/Organization.projectV2(number)` and `projectsV2(query, orderBy)`, `Repository.projectsV2`, `Issue/PullRequest.projectItems` and `projectsV2`.
  - Mutations:
    - `createProjectV2`, `updateProjectV2`, `deleteProjectV2`.
    - `addProjectV2ItemById`, `addProjectV2DraftIssue`.
    - `updateProjectV2ItemFieldValue`, `clearProjectV2ItemFieldValue`.
    - `deleteProjectV2Item`, `archiveProjectV2Item`/`unarchiveProjectV2Item`, `updateProjectV2ItemPosition`.
    - `createProjectV2Field`, `deleteProjectV2Field`.
    - `linkProjectV2ToRepository`/`unlink…`, `linkProjectV2ToTeam`.
  - `createIssue`/`createPullRequest` honor `projectV2Ids`.
  - `node()` resolves projects, items and fields.
  - REST, per docs.github.com "Projects" (projectsV2):
    - `GET /orgs/{org}/projectsV2`, `/{number}`, `/fields`, `/fields/{id}`.
    - `GET`/`POST /items`, and `GET`/`PATCH`/`DELETE /items/{id}`.
    - The same set for `/users/{user}/projectsV2`.
  - Permissions come from `bgh-projects/access.rs`.
- **Acceptance**
  - Extend `gh-compat.sh` to cover `gh project list/view/create/close/field-list/item-list/item-add/item-edit/item-archive` and `gh issue create --project`.
  - A GraphQL fixture of the `actions/add-to-project` query and mutation succeeds.
  - REST shape tests pass.
  - Empty stubs no longer return empty results when data exists.

### P14 — Directory auth: LDAP, password-login enforcement, git basic-auth throttling
- **Priority** 1 · **Wave** W1 · **Migrations** 2600–2699
- **Touches:** `bgh-accounts` (new `ldap/`, `session.rs` password login, `boot.rs`, `sso.rs` OIDC groups claim), `bgh-core` (`auth.rs` `basic_auth`, `settings.rs` `auth_providers.ldap`, `ratelimit` throttle), `bgh-admin` (`/admin/ldap/*`), web admin settings LDAP section, sign-in page
- **Scope**
  - LDAP via the `ldap3` crate:
    - Config: host, port, ldaps/StartTLS, CA, bind DN/password, user search bases, uid field, user filter, admin group, restricted-user group, attribute mapping (name, email(s), ssh keys, gpg keys), JIT provisioning, and break-glass built-in accounts for site admins.
    - Web login, git HTTP basic and API basic all authenticate through LDAP bind.
  - LDAP sync service (periodic):
    - Suspend users who are disabled or missing.
    - Update emails, names and keys.
    - Sync site admins from the admin group.
  - Team sync:
    - Generic `external_group_mappings(provider, external_group_id, team_id)`, reused by P49.
    - LDAP group DN → team members.
    - OIDC `groups` claim mapping (configurable claim name) applied at login.
  - GHES endpoints: `PATCH /admin/ldap/users/{u}/mapping`, `POST /admin/ldap/users/{u}/sync`, `PATCH /admin/ldap/teams/{id}/mapping`, `POST /admin/ldap/teams/{id}/sync`.
  - Enforce `auth_providers.password_login=false`:
    - Web password login and git and API basic auth with a built-in password are refused (403 / 401 with a clear message). PATs always work.
    - Site admins are exempt only when `password_login_admin_exempt` is true.
    - The sign-in page hides the password form.
  - Git basic-auth throttling:
    - Failed password auth is counted per login and per IP with the same throttle as web login.
    - Audit `user.failed_login` with `transport:"git"`.
    - Lock out after the threshold.
- **Acceptance**
  - Tests against an in-process fake LDAP server (`ldap3_proto`-based):
    - Login works on web and git.
    - A user disabled in LDAP is suspended by sync.
    - Team mapping adds and removes members.
    - The admin group grants site admin.
  - With `password_login=false`, password login and git password auth are rejected while PATs work.
  - 20 bad git passwords trigger the throttle and produce audit rows.
  - Admin UI smoke.

### P15 — Container registry (OCI) and Packages REST and UI
- **Priority** 1 · **Wave** W1 · **Migrations** 2700–2799
- **Touches:** new crate `bgh-packages`, `bgh-server` router (mounts `/v2/` at the host root, plus an optional dedicated hostname), `bgh-core` (package scopes and perms helper, quota), web (Packages tabs on user, org and repo, plus a package detail page)
- **Scope**
  - OCI Distribution v2:
    - `GET /v2/` returns 401 with `WWW-Authenticate: Bearer realm=…/v2/token`.
    - `/v2/token`: Basic auth with username and a PAT or `GITHUB_TOKEN` (password login not accepted) returns a JWT scoped `repository:<name>:pull,push`. Anonymous pull is allowed for public packages.
    - Blobs: `HEAD`/`GET` with Range, and `DELETE`.
    - Uploads: monolithic POST/PUT, chunked PATCH, cross-repo mount.
    - Manifests by tag or digest: `HEAD`/`GET`/`PUT`/`DELETE`, supporting OCI manifest and index and Docker schema2.
    - `tags/list` with `n`/`last`.
    - OCI 1.1 referrers.
  - Storage:
    - Content-addressed blobs in `{data_dir}/packages/blobs` with dedupe.
    - Tables `packages(id, owner_id, name, package_type, visibility, repo_id)` and `package_versions(id, package_id, digest, tags[], manifest, size, created_at, deleted_at)`.
    - Periodic GC of unreferenced blobs.
    - Count usage against the owner quota.
  - Namespacing and access:
    - Names are `{owner}/{name}[/…]`.
    - Link a package to a repo through the `org.opencontainers.image.source` label, or on first push with a repo `GITHUB_TOKEN` (`packages:write` category from P8).
    - Access is inherited from the linked repo; otherwise owner and org admins.
  - REST (GitHub shapes):
    - `/user/packages`, `/users/{u}/packages`, `/orgs/{org}/packages` (`?package_type=container`).
    - `.../packages/{type}/{name}`, plus `DELETE` and `restore`.
    - `/versions`, `/versions/{id}`, plus `DELETE` and `restore`.
  - Webhooks `package` published/updated (builder in a new payload file).
  - UI: Packages list, package page (pull command, tags, versions, size, delete), and repo sidebar "Packages".
- **Acceptance**
  - Rust integration tests modeled on the OCI distribution-spec conformance categories (push, pull, content discovery, content management). If the official conformance binary is available, run it too.
  - REST shape tests pass.
  - A `GITHUB_TOKEN` push from an `actions-e2e` job works if docker exists. Otherwise, test token scoping at the HTTP level.
  - Anonymous pull of a private package returns 401.

### P16 — Reusable workflows (`jobs.<id>.uses` / `workflow_call`)
- **Priority** 1 · **Wave** W1 · **Migrations** 2800–2899 (likely unused)
- **Touches:** `bgh-actions` (`engine.rs` expansion, `workflow/` parse and model for `on.workflow_call`, contexts), web `pages/actions` run graph
- **Scope**
  - Remove the hard-fail at `engine.rs:955-977`.
  - Resolve `./.github/workflows/x.yml` at the caller's sha, and `owner/repo/.github/workflows/x.yml@ref` from this server.
    - Access: same repo; a private or internal repo in the same org when that repo's "accessible from" setting allows it (add a simple repo setting); public repos.
  - `on.workflow_call`:
    - Typed `inputs` (required, default, type check).
    - `secrets` (explicit mapping or `secrets: inherit`).
    - `outputs` mapped to `jobs.<caller>.outputs` for downstream `needs`.
  - The caller job supports `strategy.matrix`, `needs`, `if`, `permissions` (intersected with the caller's) and `concurrency`.
  - Limits: nesting depth 4 and 20 unique called workflows, with clear errors.
  - Contexts: `github.workflow_ref`, `github.job_workflow_sha`, `inputs.*`.
  - Check run names use `caller / called-job`.
  - The run graph groups called jobs under the caller node.
- **Acceptance**
  - Fixture tests: local call, cross-repo call, `secrets: inherit`, outputs consumed downstream, matrix caller, depth-limit error, missing workflow or ref error, input type error.
  - An `actions-e2e.sh` scenario passes.
  - Run graph UI smoke.

### P17 — GitHub Apps, part 1: registration, JWT, installations, installation tokens
- **Priority** 1 · **Wave** W2 · **Migrations** 2900–2999
- **Touches:** `bgh-accounts` (new `apps/`), `bgh-core` (`auth.rs` JWT RS256 and installation tokens, `perms` via P8's route-category table, bot users), web (`/settings/apps`, `/organizations/:org/settings/apps`, `/apps/:slug` with install flow, `/settings/installations`)
- **Scope**
  - App registration, owned by a user or an org:
    - Name, slug, description, homepage, callback URLs, setup URL, webhook URL and secret (stored here; delivery in P46).
    - Repository, organization and account permissions; event subscriptions; public or private.
    - Multiple RSA private keys: generate, download PEM once, delete.
  - App JWT (RS256, `iss` = app id, max 10 min) authenticates:
    - `GET /app`, `GET /apps/{slug}`.
    - `GET /app/installations`, `GET`/`DELETE /app/installations/{id}`, suspend/unsuspend.
    - `POST /app/installations/{id}/access_tokens` with `repositories`/`repository_ids`/`permissions` narrowing.
    - `GET /orgs/{org}/installation`, `/repos/{o}/{r}/installation`, `/users/{u}/installation`.
  - Install flow UI: pick the account, then all or selected repos. A permission-upgrade acceptance step is stubbed and marked TODO for P46.
  - User endpoints: `GET /user/installations`, `/user/installations/{id}/repositories`, plus `PUT`/`DELETE` to add or remove repos. `GET /orgs/{org}/installations`.
  - Installation tokens:
    - Opaque, using the project prefix convention (for example `bghs_`), with a 1h expiry.
    - `GET /installation/repositories`, `DELETE /installation/token`.
    - Enforced through the route-category table plus the selected repo list.
    - Git over HTTPS with `x-access-token:<token>` works.
  - Each app has a bot user `{slug}[bot]` (type Bot). Actions taken with an installation token are attributed to it.
  - Separate rate-limit bucket per installation.
- **Acceptance**
  - Rust integration test of the full flow: create an app, generate a key, `GET /app` with the JWT, install on an org with one selected repo, mint a token.
  - That token reads the selected repo, gets 404 on others, and gets 403 on writes when narrowed to `contents:read`.
  - Clone with `x-access-token` works.
  - A Node script with `@octokit/auth-app` (when available) authenticates.
  - UI smoke: create and install an app.

### P18 — Metadata importer, part 1: GitHub/GHES issues, labels, milestones, releases, users
- **Priority** 1 · **Wave** W2 (after P11) · **Migrations** 3000–3099
- **Touches:** new crate `bgh-import` (job pipeline, GitHub REST client), `bgh-issues` (internal insert API with explicit numbers and timestamps), `bgh-releases` (asset import), `bgh-server` CLI (`bgh import github …`), web (`/site-admin/imports`, org settings "Import")
- **Scope**
  - Source: a GitHub.com or GHES API URL with a token.
  - The importer is rate-limit aware (conditional requests, secondary-limit backoff), resumable and idempotent through `import_mappings(source_type, source_id, local_id)`. Tables `imports` and `import_mappings`.
  - Steps:
    1. Git through P11, without the mirror flag.
    2. Repo settings: description, homepage, topics, features.
    3. Labels and milestones.
    4. Issues with **original numbers**. Set the number sequence to max(source issue or PR number) so that P51 can insert PRs with their own numbers.
    5. Issue comments, assignees, reaction counts, lock state, and key events (closed, reopened, labeled, milestoned), with original timestamps.
    6. Releases with downloaded assets.
    7. Org teams and repo team permissions (optional).
  - User mapping:
    - Match by verified email, then by login map file.
    - Otherwise create a **mannequin** placeholder account (non-login, displays the source login). Reclaim comes in P51.
  - Import mode suppresses notifications, webhooks, Actions triggers and activity (event flag `imported`), and reindexes search at the end.
  - Runs from the CLI or from admin and org UIs with a progress log.
- **Acceptance**
  - An integration test uses an in-test fake GitHub API (axum serving recorded JSON) to import a repo with issues 1, 2 and 5 (3 and 4 are PRs), comments, labels, a milestone and a release with an asset.
    - Numbers, authors (one mapped user, one mannequin) and timestamps are preserved.
    - No notifications or webhooks fire.
  - Rerunning is a no-op.
  - Killing the run mid-way and resuming finishes it.

### P19 — Deployments API and deployment statuses
- **Priority** 1 · **Wave** W2 · **Migrations** 3100–3199
- **Touches:** `bgh-actions` (new `api/deployments.rs` and service; do not touch `trigger.rs`), new payload builder file in `bgh-notify`, `bgh-issues` timeline event `deployed`, web (`/:o/:r/deployments` and `/:o/:r/deployments/activity_log`, repo sidebar "Deployments")
- **Scope**
  - Tables `deployments` and `deployment_statuses`.
  - Endpoints:
    - `GET`/`POST /repos/{o}/{r}/deployments`, filterable by `sha`, `ref`, `task` and `environment`.
    - `GET`/`DELETE /deployments/{id}`. Delete only works on inactive deployments, otherwise 422.
    - `GET`/`POST /deployments/{id}/statuses` and `GET /statuses/{id}`.
  - Deployment creation semantics:
    - `auto_merge`: merge the default branch into ref when ref is behind; 409 on conflict.
    - `required_contexts`: 409 unless all are success.
    - `transient_environment`, `production_environment`, `payload`.
    - Auto-create the environment.
    - `auto_inactive` on a new success.
    - `environment_url` and `log_url`.
  - Webhooks `deployment` created and `deployment_status` created.
  - `deployed` issue event on PRs whose head sha was deployed. The PR merge box shows "This branch was successfully deployed" (read-only data; P74 owns the MergeBox edit, so expose it through the existing status endpoint).
  - Web deployments page: per environment, latest status and history.
- **Acceptance**
  - Octokit-shape tests for every endpoint.
  - `required_contexts` returns 409. `auto_inactive` flips older deployments.
  - The webhooks are delivered.
  - The repo JSON `deployments_url` works.
  - UI smoke.

### P20 — Environment protection rules and deployment approvals
- **Priority** 1 · **Wave** W3 (after P19) · **Migrations** 3200–3299
- **Touches:** `bgh-actions` (`engine.rs` waiting state, `api/environments.rs`, `runs.rs` `pending_deployments`, `trigger.rs` deployment triggers), `bgh-pulls/src/protection.rs` (required_deployments hook), web (run page review dialog, environment settings page)
- **Scope**
  - Environment protection:
    - `wait_timer`, `reviewers` (up to 6 users or teams), `prevent_self_review`, `can_admins_bypass`.
    - `deployment_branch_policy` `{protected_branches | custom_branch_policies}`, with CRUD at `/environments/{env}/deployment-branch-policies` (branch and tag patterns).
    - REST shapes follow GitHub's `PUT /environments/{env}`.
  - Engine behavior:
    - A job naming an environment checks the branch policy; the job fails with GitHub's message if not allowed.
    - Then it waits out the timer, then waits for reviewers. Run and job status become `waiting`.
    - Environment secrets are released only after approval.
  - `GET`/`POST /actions/runs/{id}/pending_deployments` (`environment_ids`, `state` approved/rejected, `comment`) and `GET /actions/runs/{id}/approvals`.
  - Notify reviewers (email plus inbox). Webhook `deployment_review` requested/approved/rejected.
  - Jobs create deployments through P19's service: status in_progress → success/failure, with `environment.url` evaluated as an expression.
  - Triggers `on: deployment` and `on: deployment_status`.
  - Enforce `required_deployments` (ruleset and classic) in the `bgh-pulls` evaluator.
  - Web:
    - Run page banner "Review pending deployments" with a dialog.
    - Environment settings page (reviewers, timer, branch policies, admin bypass).
    - The PR shows deployment state.
- **Acceptance**
  - e2e: a production environment with a reviewer leaves the run waiting with no secrets in the env.
    - Approving via REST runs the job. Rejecting fails it.
    - The wait timer delays the job. The branch policy blocks a feature branch.
    - Deployment and status rows are created.
  - `gh run view` shows waiting.
  - A required_deployments rule blocks the merge until a deployment succeeds.

### P21 — Notification privacy, retention and polling
- **Priority** 1 · **Wave** W2 · **Migrations** 3300–3399 (indexes)
- **Touches:** `bgh-notify` (`threads.rs`, `fanout.rs` retitle, access-change listener), `bgh-core/src/sync/shapes.rs` (notification shape), `bgh-search` activity API headers, a housekeeping service
- **Scope**
  - The notification sync shape includes only rows for repos the user can currently read, using the efficient readable-repos subquery.
  - On `AccessChanged`, team or org removal, a visibility change to private, or a transfer: delete the notifications of users who lost read access, and emit sync deletes.
  - `retitle` updates only rows whose holders can still read.
  - Retention service with batched deletes and settings for each window:
    - Notifications older than 5 months.
    - `webhook_deliveries` older than 30 days (drop `payload_raw`, keep metadata 90 days).
    - `activity_events` older than 90 days.
    - Expired sessions.
  - Polling headers:
    - `/notifications` and `/repos/{o}/{r}/notifications`: `Last-Modified` (max updated_at), `If-Modified-Since` → 304, `X-Poll-Interval: 60`.
    - Events API endpoints: `X-Poll-Interval` and `Last-Modified`.
- **Acceptance**
  - After a collaborator is removed, their bootstrap and REST contain no rows for that repo, and a later title change does not reach them.
  - Retention deletes rows older than the threshold and keeps newer ones.
  - `If-Modified-Since` returns 304 with `X-Poll-Interval`.

### P22 — Pulls and checks REST correctness
- **Priority** 1 · **Wave** W2 · **Migrations** 3400–3499 (unused)
- **Touches:** `bgh-pulls` (`checks.rs`, `statuses.rs`, `commits.rs` node_id, `json.rs`/`reviews.rs`/`comments.rs` BodyFormat), `bgh-search/src/commits.rs` node_id, `bgh-server` (HTML-host `.diff`/`.patch` routes before the SPA fallback)
- **Scope**
  - Add `Link` headers and correct `total_count` to:
    - `GET /commits/{ref}/check-runs`
    - `/check-suites/{id}/check-runs`
    - `/commits/{ref}/check-suites`
    - `/commits/{ref}/status` (statuses capped per GitHub, with pagination)
    - `/commits/{ref}/statuses`
  - Encode commit `node_id` as `"{repo_id}:{sha}"` everywhere, including PR commits and commit search.
  - Honor `Accept: application/vnd.github.{html,text,full}+json` for PRs, reviews and review comments (`body_html`/`body_text`).
  - Serve `/{o}/{r}/pull/{n}.diff` and `.patch`, `/{o}/{r}/commit/{sha}.diff` and `.patch`, and `/{o}/{r}/compare/{a}...{b}.diff` as text, with read access checks.
- **Acceptance**
  - 45 check runs: following `Link` returns all of them, and `octokit.paginate` (if available) or a Link-follow test agrees.
  - Node ids from `/pulls/n/commits`, `/commits/sha` and `/search/commits` are equal and resolve through GraphQL `node()`.
  - The `full` media type returns `body_html`.
  - `curl /o/r/pull/1.diff` returns the diff, and a private repo needs auth.

---

## 3. Priority 2 packages

### P23 — Rulesets, part 2 backend: org rulesets, push and metadata rules, evaluate mode, rule suites
- **Priority** 2 · **Wave** W2 · **Migrations** 3500–3599
- **Touches:** `bgh-repos` (`rulesets.rs`, `protection.rs` shared loader used by P3's evaluator), `bgh-git` pre-receive object enumeration (from P2), `bgh-accounts` (`/orgs/{org}/rulesets` routes can live in `bgh-repos`)
- **Scope**
  - `/orgs/{org}/rulesets` CRUD, with `conditions.repository_name` include/exclude (fnmatch, `~ALL`, `protected`) and `repository_id`.
  - `GET /repos/{o}/{r}/rules/branches/{b}` includes org rules (`ruleset_source_type: Organization`).
  - New rule types:
    - Push rules evaluated in pre-receive: `file_path_restriction`, `max_file_size`, `file_extension_restriction`, `max_file_path_length`.
    - Metadata rules with operators starts_with/ends_with/contains/regex and negate: `commit_message_pattern`, `commit_author_email_pattern`, `committer_email_pattern`, `branch_name_pattern`, `tag_name_pattern`.
    - Stored, with enforcement elsewhere: `merge_queue` (P39), `required_deployments` (P20), `workflows` (P30), `code_scanning` (P66).
  - Bypass actor types DeployKey, Integration, OrganizationAdmin, RepositoryRole and Team.
  - Push rejections use `GH013: Repository rule violations found for <ref>` with per-rule lines.
  - `enforcement: evaluate` doesn't block but records results.
  - Rule suites: store each push and merge evaluation, served at `GET /repos/{o}/{r}/rulesets/rule-suites[/{id}]` and the org variant.
  - Map the legacy `/repos/{o}/{r}/tags/protection` endpoints onto tag rulesets.
- **Acceptance**
  - A REST fixture of a Terraform `github_organization_ruleset` payload round-trips.
  - Pushes violating `max_file_size` or `commit_message_pattern` are rejected with GH013. In evaluate mode, the push is allowed and recorded in rule-suites.
  - A DeployKey bypass works.
  - `gh ruleset list/view --org` works.

### P24 — Rulesets UI (repo and org)
- **Priority** 2 · **Wave** W2 (built against GitHub's REST contract, alongside P23) · **Migrations** 3600–3699 (unused)
- **Touches:** `web/src/pages/repo-settings` (new Rules section), `web/src/pages/orgsettings` (Rulesets), branches page badges, mock backend
- **Scope**
  - Rulesets list, new and edit pages for repos and orgs:
    - Name, enforcement (active/evaluate/disabled).
    - Bypass list with an actor picker.
    - Targets: include/exclude patterns with a live preview of matching branches or tags; for orgs, repository targeting.
    - Forms for every rule type, including pull_request parameters, required checks with an app picker, and push and metadata patterns.
  - Rule insights page: rule-suites list filtered by ref, actor and result, with detail.
  - JSON import and export.
  - Replace the "use rulesets" hint in `BranchesSettings` with a link.
  - The branches list shows which rulesets protect each branch.
- **Acceptance**
  - Vitest for the form serialization of each rule type (matching the GitHub JSON).
  - Smoke: create a `release/*` ruleset in the UI and confirm a push is blocked.
  - Mock backend supports all of it.

### P25 — Commit and tag signature verification, SSH signing keys, required_signatures, web-flow signing
- **Priority** 2 · **Wave** W3 · **Migrations** 3700–3799
- **Touches:** `bgh-git` (new `signing.rs`: parse `gpgsig`, sign server commits), `bgh-repos` (`gitjson.rs` `Verification`, `protection.rs` push enforcement), `bgh-pulls` (`commits.rs`, required_signatures in the evaluator, signed merge/squash/rebase/update-branch/suggestion commits), `bgh-accounts` (`/user/ssh_signing_keys` and the GPG key email binding), web (Verified badges on commit list, commit page and PR commits; SSH signing keys settings)
- **Scope**
  - Verification:
    - OpenPGP via `pgp` (rPGP) against the signer's uploaded GPG keys, including subkeys and expiry.
    - SSH signatures via `ssh-key`/sshsig against `/user/ssh_signing_keys`.
    - Email must be a verified email of the key owner.
    - GitHub reason codes: `valid`, `unsigned`, `unknown_key`, `bad_email`, `unverified_email`, `no_user`, `unknown_signature_type`, `expired_key`, `not_signing_key`, `malformed_signature`, `invalid`, `gpgverify_error`.
    - Cache by `(object sha)` in a `signature_verifications` table, invalidated when keys change.
  - `/user/ssh_signing_keys` CRUD and `/users/{u}/ssh_signing_keys`.
  - Web-flow key:
    - Generate the server key at `{data_dir}/signing/`, publish it at `/web-flow.gpg`, and use it as a system key that verifies as valid.
    - Sign every server-made commit (web edits, merges, squash, rebase, update-branch, template generation, suggestions) instead of `--no-gpg-sign`.
  - Enforce `required_signatures` (classic and ruleset):
    - On push: every new commit must be verified, or GH013/GH006 style rejection.
    - On merge: every PR commit must be verified, and the merge commit is signed by web-flow.
  - GraphQL `Commit.signature` populated.
- **Acceptance**
  - Fixture commits signed with test GPG and SSH keys verify as valid.
  - A wrong email gives `bad_email`. An unknown key gives `unknown_key`.
  - Web merges are verified.
  - With required_signatures on, an unsigned push is rejected and a signed push is accepted.
  - The UI shows Verified badges.

### P26 — Actions triggers completeness, repository_dispatch, merge-ref PR runs, check re-run, job concurrency, badge.svg
- **Priority** 2 · **Wave** W2 · **Migrations** 3800–3899 (likely unused)
- **Touches:** `bgh-actions` (`trigger.rs` owner in W2, `engine.rs` job concurrency, new `/dispatches` route, a CheckRunRerequested/CheckSuiteRerequested listener, `web.rs` badge route), `bgh-pulls` (create and maintain `refs/pull/N/merge` merge commits)
- **Scope**
  - Fire these triggers, with `types:` filtering:
    - `workflow_run` (requested/completed/in_progress, `workflows:` filter, branches).
    - `repository_dispatch` via a new `POST /repos/{o}/{r}/dispatches` (`event_type` and `client_payload`, 204).
    - `pull_request` labeled, unlabeled, ready_for_review, edited, converted_to_draft, review_requested, review_request_removed, assigned, unassigned, locked.
    - `pull_request_target` with the same types.
    - `pull_request_review` and `pull_request_review_comment`.
    - `create`, `delete`.
    - `issues` labeled, unlabeled, assigned, unassigned, milestoned, demilestoned, pinned, transferred.
    - `issue_comment` edited and deleted.
    - `check_run`, `check_suite`.
    - `release` (all types).
    - `label`, `milestone`, `watch`, `fork`, `public`, `gollum`.
  - PR runs:
    - `bgh-pulls` keeps `refs/pull/N/merge` as a real test-merge commit, updated on head or base change and absent when the PR conflicts.
    - `GITHUB_SHA` is the merge sha and `GITHUB_REF` is `refs/pull/N/merge`. If the PR conflicts, the run doesn't trigger, as on GitHub.
  - Re-running from Checks (`rerequest`) re-runs the Actions job or suite instead of leaving the check queued.
  - Enforce job-level `concurrency` (group and cancel-in-progress).
  - `GET /{o}/{r}/actions/workflows/{file}/badge.svg?branch=&event=` serves an SVG badge (passing/failing/no status) with no-cache headers.
- **Acceptance**
  - One trigger test per new event family.
  - `gh api -X POST repos/o/r/dispatches` starts a `repository_dispatch` workflow.
  - A `workflow_run` workflow runs after CI completes.
  - A PR run's sha equals a merge commit whose parents are base and head.
  - Re-run from the Checks tab moves the check queued → completed.
  - Job concurrency cancels the older job.
  - The badge renders for passing and failing runs.

### P27 — Actions cache and toolkit runtime services
- **Priority** 2 · **Wave** W3 · **Migrations** 3900–3999
- **Touches:** `bgh-actions` (new `cache/` and `results/` service modules, runner env in `runner/job.rs`, `/actions/caches` API), web Actions caches page
- **Scope**
  - Set `ACTIONS_RUNTIME_URL`, `ACTIONS_RUNTIME_TOKEN` (a job-scoped JWT), `ACTIONS_RESULTS_URL` and `ACTIONS_CACHE_URL` in the job environment.
  - Implement the legacy cache v1 protocol (`_apis/artifactcache/cache`, `caches`, `PATCH` chunks, commit) and the twirp `github.actions.results.api.v1.CacheService` (v2).
    - The v2 client uploads to signed URLs with the Azure Blob client, so the server must emulate the Azure Blob subset: Put Blob, Put Block, Put Block List, and Get Blob with Range.
  - Implement the twirp `ArtifactService` so `@actions/artifact` v2 used by third-party actions works, alongside the native interception of upload/download-artifact.
  - Cache scoping follows GitHub:
    - The key is the ref, falling back to the base or default branch.
    - Exact-key and `restore-keys` prefix matching.
    - Version hash.
    - Per-repo size limit (default 10 GB, configurable) with LRU eviction, and expiry after 7 days unused.
  - Native `actions/cache` interception now restores and saves for real.
  - REST:
    - `GET /repos/{o}/{r}/actions/caches`, `DELETE ?key=` and `/{id}`.
    - `GET /actions/cache/usage`, plus org usage.
  - Web caches list on the Actions page.
- **Acceptance**
  - Integration: a workflow with `actions/cache` misses on the first run and hits on the second.
  - `actions/setup-node` with `cache: npm` works when the network is available, or a fixture JS action using `@actions/cache` works.
  - `gh cache list/delete` works.
  - Eviction test.
  - Blob API conformance tests for Put Block and Put Block List.

### P28 — Actions OIDC id-token
- **Priority** 2 · **Wave** W4 · **Migrations** 4000–4099 (likely unused)
- **Touches:** `bgh-actions` (`server.rs`, runner env, new `oidc.rs`), `bgh-core` (signing key storage), the `bgh-server` route
- **Scope**
  - Issuer `https://<host>/_services/token` (GHES convention), serving `/.well-known/openid-configuration` and a JWKS endpoint.
  - RS256 keys stored in `data_dir`, with rotation that keeps the old key in the JWKS.
  - Jobs with `permissions: id-token: write` get `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`. The request endpoint takes `?audience=`.
  - Claims match GitHub: `sub` (`repo:o/r:ref:…`, `environment:…`, `pull_request`), `repository`, `repository_id`, `repository_owner`, `ref`, `sha`, `workflow`, `job_workflow_ref`, `run_id`, `run_attempt`, `actor`, `event_name`, `environment`, `runner_environment`, …
  - `GET`/`PUT /repos/{o}/{r}/actions/oidc/customization/sub` and the org variant.
- **Acceptance**
  - A test job fetches a token, and the token validates against the JWKS with the expected claims.
  - The custom `sub` template is applied.
  - Without `id-token: write`, no variables are set.
  - The docs show AWS, GCP and Azure trust setup.

### P29 — Runners: OS and arch, labels, groups, site runners, JIT config, admin runner UI
- **Priority** 2 · **Wave** W3 · **Migrations** 4100–4199
- **Touches:** `bgh-actions` (`runner/job.rs` env, `web.rs` register, `api/runners.rs`, new runner-groups API), `bgh-admin` (site runners API), web (org settings runner groups, `/site-admin/actions/runners` with queue view)
- **Scope**
  - `bgh-runner` detects and reports OS and arch. `RUNNER_OS` is Linux/Windows/macOS and `RUNNER_ARCH` is X86/X64/ARM/ARM64.
  - `system_labels` are set from the registration (for example `self-hosted, macOS, ARM64`).
  - Runner groups for orgs and the site:
    - Visibility all or selected repos, optional workflow allowlist.
    - `/orgs/{org}/actions/runner-groups` CRUD plus `/repositories` and `/runners` sub-resources, in GitHub shapes.
  - Site-level runners:
    - Admin can mint registration tokens with both `repo_id` and `org_id` NULL.
    - A site runner-groups API under `/_bgh/admin/actions` (the GHES `/enterprises/{e}/actions/runners` shape when cheap).
  - `POST /repos/{o}/{r}/actions/runners/generate-jitconfig` and the org variant give `bgh-runner` ephemeral one-job runners.
  - Admin UI: all runners with status, labels and busy state, the queued-jobs view, and remove runner.
- **Acceptance**
  - A test registers a runner reporting macOS/ARM64. A `runs-on: [self-hosted, macOS]` job matches it and `RUNNER_OS=macOS` inside the job.
  - A runner group restricted to repo A won't take repo B's jobs.
  - A JIT-config runner exits after one job.
  - Admin UI smoke.

### P30 — Actions policies and settings (repo, org, site), fork-PR approval, retention, settings nav
- **Priority** 2 · **Wave** W4 · **Migrations** 4200–4299
- **Touches:** `bgh-actions` (`api/permissions.rs`, `/approve`, retention in `services.rs`, `logs.rs` expiry), `bgh-admin` (site Actions policy section), web (repo settings "Actions → General", org settings Actions, site-admin Actions; add Secrets and variables, Environments and Runners to the repo and org settings navs; wrap org Actions routes in `OrgSettingsLayout`)
- **Scope**
  - REST:
    - `GET`/`PUT /repos/{o}/{r}/actions/permissions` (`enabled`, `allowed_actions` all/local_only/selected) and `/selected-actions` (`github_owned_allowed`, `verified_allowed`, `patterns_allowed`).
    - `/permissions/workflow` (`default_workflow_permissions`, `can_approve_pull_request_reviews`), feeding P8's defaults.
    - `/permissions/access` (for reusable workflows, P16).
    - Org equivalents, with `enabled_repositories` all/none/selected.
    - Site policy that caps org and repo settings.
  - The engine refuses disallowed `uses:` with a clear annotation.
  - Fork PR approval:
    - Policy: first-time contributors, or all outside collaborators.
    - Runs from such forks get status `action_required` until `POST /actions/runs/{id}/approve`, with a UI button on the run and on the PR Checks tab.
  - Retention:
    - `/actions/permissions/artifact-and-log-retention` (repo and org).
    - Log expiry, plus a sweep of logs and artifacts belonging to deleted repos.
  - Ruleset `workflows` rule (required workflows) as a stretch goal: run the listed workflow from the source repo on PRs and require its check.
- **Acceptance**
  - Allowed-actions `local_only` fails a job that uses a github.com action.
  - A first-time fork PR shows `action_required`. Approving runs it.
  - Read-only default permissions apply.
  - The retention job deletes old logs.
  - The settings navs show the new sections.
  - `gh api` checks of the permissions endpoints match GitHub shapes.

### P31 — Repository Insights: stats, traffic, community profile, Insights UI
- **Priority** 2 · **Wave** W2 · **Migrations** 4300–4399
- **Touches:** `bgh-repos` (`stats.rs` and a new `traffic.rs`; clone counting in `git_http.rs`/ssh as a one-line hook), web (new `pages/repo/insights/*` replacing the `pulse` placeholder)
- **Scope**
  - `/stats/contributors`, `commit_activity`, `code_frequency`, `participation` and `punch_card`:
    - Computed by a job and cached by default-branch head sha.
    - Return 202 while computing, as GitHub does. `code_frequency` returns 422 for repos with 10k+ commits.
  - Traffic:
    - Record clones (git fetch with no `have` lines), unique per user or IP hash.
    - Record page views through a `/_bgh` beacon from the SPA with referrer and path.
    - Endpoints: `/traffic/views`, `/traffic/clones` (`per=day|week`, 14 days), `/traffic/popular/paths` and `/traffic/popular/referrers`. Push access required.
  - `GET /repos/{o}/{r}/community/profile`: health percentage and files (README, CODE_OF_CONDUCT, CONTRIBUTING, LICENSE via P33 if merged, else null, issue and PR templates, SECURITY).
  - `GET /repos/{o}/{r}/activity` (the push and ref-change log).
  - Insights UI:
    - Pulse (period summary of PRs, issues and commits).
    - Contributors graph.
    - Commit activity, code frequency, traffic (admin).
    - Forks list and network list.
    - Community standards checklist.
    - Lightweight SVG charts in a lazy chunk, following the dataviz skill guidance.
- **Acceptance**
  - Shape tests for each endpoint, including the 202-then-200 flow.
  - A clone increments the traffic counters.
  - The Insights pages render in mock and real modes.
  - The bundle budget holds because charts are lazy.

### P32 — Commit comments
- **Priority** 2 · **Wave** W2 · **Migrations** 4400–4499
- **Touches:** `bgh-repos` (new `commit_comments.rs`), a payload builder file and notification fanout in `bgh-notify`, `bgh-admin/src/stats.rs` (the real count), web `pages/commits/CommitPage.tsx` (thread plus inline line comments in the commit diff)
- **Scope**
  - REST:
    - `GET`/`POST /repos/{o}/{r}/commits/{sha}/comments` (`body`, `path`, `position`/`line`).
    - `GET /repos/{o}/{r}/comments`, `GET`/`PATCH`/`DELETE /comments/{id}`.
    - Reactions at `/comments/{id}/reactions`.
    - Media types, and `/commits/{sha}/pulls`.
  - Webhook `commit_comment` created.
  - Notifications to the commit author and to mentions.
  - The activity event `CommitCommentEvent`.
  - `comments_url` resolves.
  - UI: a comment thread at the bottom of the commit page and line comments in the commit diff (reusing the review-comment components), with reactions.
- **Acceptance**
  - Shape tests pass. The webhook is delivered. The author is notified.
  - UI smoke: comment on a line and see it after reload.

### P33 — Repo metadata endpoints: licenses, gitignore, license detection, create templates, repositories by id, small REST gaps
- **Priority** 2 · **Wave** W2 · **Migrations** 4500–4599 (likely unused)
- **Touches:** `bgh-repos` (`create.rs`, new `licenses.rs`/`gitignore.rs`, `gitdb.rs` short SHAs, `commits.rs`), `bgh-search` (`license:` qualifier), `bgh-graphql` (`licenseInfo`), web `NewRepoPage` (enable the pickers)
- **Scope**
  - Embed the choosealicense data (common licenses) and the github/gitignore templates (CC0).
  - Endpoints: `GET /licenses`, `/licenses/{key}`, `/gitignore/templates` and `/gitignore/templates/{name}`.
  - License detection with `askalono` on push to the default branch:
    - Writes `license_spdx_id`.
    - The repo JSON `license` object is populated.
    - `GET /repos/{o}/{r}/license` (content plus the license object).
    - GraphQL `licenseInfo`, and search `license:`.
  - `POST /user/repos` and `/orgs/{org}/repos` honor `gitignore_template`, `license_template` (implies `auto_init`) and `team_id`.
  - `GET /repositories/{id}` and `GET /repositories?since=`.
  - `GET /repos/{o}/{r}/commits/{sha}/branches-where-head`.
  - Short (≥7) SHAs in `git/blobs|commits|tags/{sha}`.
- **Acceptance**
  - `gh repo create x --gitignore Go --license mit --team t` creates both files and the team grant.
  - `gh repo license list/view` and `gh repo gitignore list/view` work.
  - An MIT repo reports `license.spdx_id=MIT`.
  - `/repositories/{id}` matches `/repos/o/r`.
  - Shape tests pass.

### P34 — API compatibility headers and root endpoints
- **Priority** 2 · **Wave** W2 · **Migrations** 4600–4699 (unused)
- **Touches:** `bgh-server` (`api_headers`, `etag`, fallbacks, layer order), `bgh-core` (`auth.rs` accepted scopes, `ratelimit.rs`), `bgh-accounts/src/meta.rs`, `bgh-graphql/src/lib.rs` (meta), `bgh-core::markdown` exposure
- **Scope**
  - Headers:
    - `X-GitHub-Enterprise-Version` on every API response.
    - `X-GitHub-Request-Id` (same value as `x-request-id`).
    - `X-Accepted-OAuth-Scopes` on scope-gated endpoints.
    - Validate `X-GitHub-Api-Version`: 400 for unsupported versions, in GitHub's shape.
  - Wrong method on a known path returns a JSON 404 with `documentation_url` (`method_not_allowed_fallback`).
  - Conditional requests:
    - Add `Last-Modified`/`If-Modified-Since` support to the ETag layer for responses that carry `updated_at`.
    - Compute the ETag only when the response is cacheable and below a size threshold, or when `If-None-Match` is present.
    - 304s do not consume rate limit (reorder the layers).
  - Root endpoints:
    - `POST /markdown` (`mode`, `context`) and `/markdown/raw`.
    - `GET /emojis`: gemoji names → locally served image URLs (bundle a permissively licensed set with attribution).
    - `GET /zen`, `/octocat`, `/versions`.
  - `/meta` returns `ssh_key_fingerprints` and `ssh_keys` from the host keys.
  - Root URLs (gists, feeds, authorizations) point only at implemented endpoints, or are omitted until P72/P80 land.
- **Acceptance**
  - Renovate's `HEAD /api/v3/` returns `X-GitHub-Enterprise-Version`.
  - A 304 doesn't decrement `X-RateLimit-Remaining`.
  - `DELETE` on a GET-only path returns a JSON 404.
  - `POST /markdown` renders GFM with context links.
  - Tests for each header.

### P35 — Markdown rendering parity (client and server)
- **Priority** 2 · **Wave** W2 · **Migrations** 4700–4799 (unused)
- **Touches:** `web/src/ui/markdown/render.ts` and `Markdown.tsx`, `bgh-core/src/markdown.rs`, a shared generated emoji JSON, repo autolinks exposed to the client (sync shape or cached fetch), new `/_bgh/camo` image proxy
- **Scope**
  - Client renderer:
    - GFM alerts (`> [!NOTE]` and friends), footnotes, heading anchors.
    - Commit SHA links, `owner/repo#N`, `GH-N`, and URL→`#N` shortening for issue and PR URLs on this host.
    - Repo custom autolinks.
    - The full gemoji shortcode set from a shared JSON, also used by comrak.
    - Fenced-code syntax highlighting in a lazy chunk, or via server `/_bgh/render` for long blocks.
    - Mermaid and math (`$…$`, `$$…$$`, ```` ```math ````) in lazy chunks.
    - `<img loading="lazy" decoding="async">`.
  - Task lists:
    - For users with write access (or the author), clicking a checkbox rewrites the nth task item in the source and saves through the existing optimistic edit mutation.
    - Read-only users still see the checkbox disabled.
  - Server (comrak): apply the repo autolinks, emoji shortcodes and math so API `body_html` and emails match.
  - Camo-style proxy:
    - Rewrite external `<img src>` to `/_bgh/camo/{hmac}/{hex-url}`, with an HMAC key in `data_dir`, a size and type limit, and the SSRF guard.
    - Controlled by a site setting (default on).
- **Acceptance**
  - A golden test corpus renders identically client and server for alerts, footnotes, autolinks, emoji, refs and math markers.
  - Ticking a checkbox updates the issue body.
  - External images load through camo.
  - Mermaid and KaTeX chunks load only when used; the bundle budget holds.

### P36 — Account security: WebAuthn and passkeys, 2FA policies, sudo mode, TOTP encryption, token expiry
- **Priority** 2 · **Wave** W2 · **Migrations** 4800–4899
- **Touches:** `bgh-accounts` (`twofa.rs`, `session.rs`, `orgs.rs`, `tokens.rs`, `keys.rs`, new `webauthn.rs`), `bgh-core` (`settings.rs` site 2FA policy, mail templates), web (settings security, login page, org settings 2FA toggle)
- **Scope**
  - WebAuthn with `webauthn-rs`:
    - Security keys as a second factor.
    - Passkeys for passwordless sign-in (discoverable credentials).
    - Manage, rename and delete keys.
    - Recovery codes still apply.
  - Org 2FA requirement:
    - `PATCH /orgs/{org}` accepts `two_factor_requirement_enabled` (owner must have 2FA).
    - Enabling it removes non-compliant members and outside collaborators, records them for reinstatement, and notifies them.
    - Invitations to users without 2FA are blocked (422).
    - Org settings toggle.
  - Site policy `auth.require_2fa`: users without 2FA are forced into setup after login (except API PATs).
  - Sudo mode:
    - Re-auth with password, TOTP or WebAuthn, valid 2h, before creating PATs, SSH or GPG keys, OAuth or GitHub apps, changing emails, deleting repos, or transferring.
    - API returns 401 with a clear message when a sudo-less session cookie is used.
  - Encrypt TOTP secrets at rest (with a migration that re-encrypts existing ones).
  - PAT expiry:
    - Emails 7 days and 1 day before expiry.
    - `GitHub-Authentication-Token-Expiration` response header.
- **Acceptance**
  - WebAuthn registration and assertion tested with a software authenticator (`webauthn-rs` test utilities).
  - Enabling the org requirement removes a no-2FA member.
  - Creating a PAT without recent sudo is refused.
  - The DB contains no plaintext TOTP secrets.
  - The expiry email is queued, and the expiry header is present.

### P37 — Diff viewer: syntax highlighting, context expansion, rich and image diffs, file actions, inline annotations
- **Priority** 2 · **Wave** W3 · **Migrations** 4900–4999 (unused)
- **Touches:** `web/src/components/diff/DiffView.tsx` and `FileHeader`, `web/src/pages/pulls/FilesTab.tsx` (file actions only), `bgh-pulls/src/web.rs` or `bgh-repos` (a `/_bgh` blob-lines endpoint by sha and range)
- **Scope**
  - Syntax highlighting:
    - Tokenize the old and new blobs with the server highlighter (`/_bgh/render/blob` by blob sha, cached).
    - Map tokens onto diff lines, with lazy per-file requests while virtualized.
    - Fall back to plain text for huge files.
  - Context expansion:
    - Hunk rows get "expand up", "expand down" and "expand all" (20 lines per click) using a lines-by-range endpoint.
    - Works in split and unified views.
  - Binary and image diffs:
    - Images get 2-up, swipe and onion-skin views via the raw URLs at both shas.
    - Other binaries show sizes.
  - Rich Markdown diff: rendered before/after toggle.
  - File header actions: "View file" at head, "Edit file" (web editor on the head branch, if the user may push), copy path.
  - Show check-run annotations inline at their lines (synced or fetched for the head sha).
  - Collapse generated files by default (`linguist-generated` from `.gitattributes` once P78 lands; filename heuristics now).
- **Acceptance**
  - Vitest for line-to-token mapping and the expansion state.
  - Smoke: highlighting appears, expansion works on a 1000-line file, an image diff shows the swipe view, annotations appear on the right line.
  - Scroll performance stays at 60fps on a 5k-line diff (manual profile).

### P38 — Review workflow: commit-range diffs, "changes since your last review", server-side viewed state, batch suggestions
- **Priority** 2 · **Wave** W3 · **Migrations** 5000–5099
- **Touches:** `bgh-pulls` (`web.rs` range diff endpoint, viewed-files table and endpoints, suggestion-apply endpoint), `web/src/pages/pulls/FilesTab.tsx`, `viewed.ts`, `ReviewThread.tsx`, `web/src/sync/pullMutations.ts`
- **Scope**
  - Files tab commit picker: all changes, one commit, a range, or "changes since your last review" (the head sha at the viewer's last submitted review). Backend range diff `/_bgh/repos/{o}/{r}/pulls/{n}/files?base_sha=&head_sha=`.
  - Viewed state:
    - Table `pull_viewed_files(pull_id, user_id, path, blob_sha)`.
    - A file becomes unviewed when its blob changes.
    - Endpoints to mark and unmark.
    - Exposed in sync; migrate the existing localStorage state once.
    - P45 adds the GraphQL fields.
  - Suggestions:
    - "Add suggestion to batch" and "Commit suggestions" go through a server endpoint that applies N suggestions in one commit on the head branch, preserving line endings, with no size limit beyond git.
    - Editable commit message and description.
    - Correct `Co-authored-by: Name <email>` trailers for each suggestion author.
    - Applied threads are auto-resolved.
- **Acceptance**
  - Selecting one commit shows only its changes. "Since last review" shows only post-review changes.
  - Viewed state persists across browsers, and resets when the file changes.
  - A batch of 3 suggestions makes 1 commit with 3 trailers, keeps CRLF intact, and resolves the threads.
  - Backend tests for the endpoints.

### P39 — Merge queue
- **Priority** 2 · **Wave** W3 (after P23 and P26) · **Migrations** 5100–5199
- **Touches:** `bgh-pulls` (new `merge_queue/` module, `automerge.rs`, evaluator hook), `bgh-actions` (`merge_group` trigger, an additive arm), `bgh-graphql` (new mutation and model files), web `MergeBox.tsx` (owner in W3) and a new queue page
- **Scope**
  - The `merge_queue` ruleset rule parameters (`merge_method`, `max_entries_to_build`, `min/max_entries_to_merge`, `grouping_strategy`, `check_response_timeout_minutes`) gate merges. With the rule active, a direct merge is refused and the PR must be enqueued.
  - Queue processing service:
    - Build temp branches `gh-readonly-queue/{base}/pr-{n}-{sha}` that stack queued PRs on the latest base.
    - Emit a `merge_group` checks_requested event, which Actions triggers on and which creates check suites.
    - Wait for required checks on the group sha, then fast-forward base and mark PRs merged.
    - On failure, drop the failing PR, rebuild the rest, and notify.
  - GraphQL:
    - `enqueuePullRequest`, `dequeuePullRequest`.
    - `Repository.mergeQueue(branch)` with `entries`, `position`, `state`, `estimatedTimeToMerge`.
    - `PullRequest.isInMergeQueue` and `mergeQueueEntry`.
  - Webhook `merge_group` (checks_requested/destroyed).
  - UI:
    - Merge box "Merge when ready" becomes "Add to merge queue", with position and status, and "Remove from queue".
    - `/:o/:r/queue/:branch` page listing entries.
- **Acceptance**
  - Integration test: 3 PRs enqueued. CI (an Actions `merge_group` workflow or posted statuses) passes for the group, and all 3 merge in order. A failing PR is ejected and the others still merge.
  - `gh pr merge --auto` enqueues when the queue is required.
  - GraphQL shape tests pass.

### P40 — Conflict resolution, revert PR, rebase update-branch, squash co-authors
- **Priority** 2 · **Wave** W4 · **Migrations** 5200–5299 (unused)
- **Touches:** `bgh-pulls` (`mergeability.rs` conflict details, `merge.rs` update-branch rebase and squash trailers, new `revert.rs`, conflict-resolve endpoint), `bgh-git` (merge-tree conflict info), `bgh-graphql` (`revertPullRequest`, `updatePullRequestBranch` `updateMethod`, `revertUrl`), web `MergeBox.tsx` (owner in W4) and a new conflict editor page
- **Scope**
  - Mergeability keeps the conflicting file list from `merge-tree --write-tree --name-only`.
  - The merge box lists the conflicting files and shows command-line instructions, as GitHub does.
  - "Resolve conflicts" web editor:
    - Shows conflict markers per file and validates that no markers remain.
    - Commits a merge commit on the head branch (only if the head is in the same repo or maintainer edits are allowed).
    - Simple text conflicts only; binary and rename conflicts fall back to the CLI.
  - Revert:
    - `POST /_bgh/…/pulls/{n}/revert` and GraphQL `revertPullRequest` create branch `revert-{n}-{head}` with the revert commit (via merge-tree) and open a PR titled "Revert \"…\"".
    - `revertUrl` is populated, and the merged box gets a "Revert" button.
  - Update branch:
    - `update_method` merge or rebase (REST `PUT /update-branch` body and GraphQL `updateMethod`).
    - Rebase rewrites the head branch commits on top of base with force-update and records `head_ref_force_pushed`.
    - Honor `allow_update_branch`: show the button whenever base is ahead and the setting is on.
  - Squash merges append `Co-authored-by:` trailers for every distinct non-PR-author commit author (and existing co-author trailers), deduplicated.
- **Acceptance**
  - The conflicting-files list matches git.
  - Resolving in the web editor produces a mergeable PR.
  - Revert creates a PR whose diff is the inverse.
  - Rebase update gives linear history.
  - A squash of 2 authors carries a trailer.
  - `gh pr revert` works if available, otherwise the GraphQL test covers it.

### P41 — Issue types, issue dependencies, close as duplicate
- **Priority** 2 · **Wave** W3 · **Migrations** 5300–5399
- **Touches:** `bgh-issues` (types, dependencies, duplicate close), `bgh-accounts` (`/orgs/{org}/issue-types`), `bgh-core` sync shapes (additive fields), `bgh-graphql` (`model/issue.rs` fields and the `mutation/issues.rs` inputs `issueTypeId` and `duplicateIssueId`), web (`IssueSidebar`, `Timeline` close menu region, `filters.ts`, org settings issue types page)
- **Scope**
  - Issue types:
    - Org-defined (name, color, description, enabled), seeded with Task, Bug and Feature.
    - `/orgs/{org}/issue-types` CRUD.
    - Issue REST `type` field, set with `PATCH type`.
    - `type:` filter in the list and in server search.
    - Templates (`type:`) applied on create.
    - Timeline events `issue_type_added`, `issue_type_changed` and `issue_type_removed`.
    - GraphQL `issueType` and `issueTypeId` inputs.
  - Dependencies:
    - `/repos/{o}/{r}/issues/{n}/dependencies/blocked_by` (GET/POST/DELETE) and `/blocking` (GET).
    - Cross-repo allowed when readable; cycle prevention.
    - Timeline events `blocked_by_added/removed` and `blocking_added/removed`.
    - Sidebar "Relationships" section.
    - `is:blocked` and `is:blocking` filters.
    - Blocked badge in the list.
  - Close as duplicate:
    - Close menu option with a duplicate-of picker.
    - `state_reason: duplicate` preserved (remove the downgrade in `sync/shapes.rs:185`).
    - Store `duplicate_of` and emit `marked_as_duplicate`.
    - GraphQL `closeIssue(duplicateIssueId)`.
- **Acceptance**
  - Shape tests for the issue-types and dependencies endpoints.
  - A template `type: Bug` applies the type.
  - A dependency cycle returns 422.
  - Duplicate close shows "Closed as duplicate of #N", and REST `state_reason` is `duplicate`.
  - UI smoke.

### P42 — Comment moderation and edit history
- **Priority** 2 · **Wave** W3 · **Migrations** 5400–5499
- **Touches:** `bgh-issues` (comments, issue delete, `content_edits`), `bgh-pulls` (review comments use the same minimize and edits path), `bgh-graphql` (`minimizeComment`/`unminimizeComment`, `deleteIssue`, `userContentEdits`, `isMinimized`, `minimizedReason`, `viewerCanMinimize`), web (`Timeline` comment menu region, edit history dropdown, issue delete in the sidebar)
- **Scope**
  - Hide or minimize comments (issue, PR and review comments) with a reason: spam, abuse, off-topic, outdated, duplicate or resolved. Triage access or above. Collapsed rendering with "Show comment".
  - Delete an issue:
    - Repo admins (or org owners, honoring `members_can_delete_issues` once P48 lands).
    - GraphQL `deleteIssue`, UI "Delete issue" with confirmation.
    - The number stays reserved and returns 404/410.
  - Edit history:
    - `user_content_edits(target, editor, diff/body snapshot, created_at)` for issue and PR bodies and all comment kinds.
    - "edited ▾" dropdown showing revisions. Authors or admins can delete a revision.
    - GraphQL `userContentEdits`.
- **Acceptance**
  - Minimize via UI and GraphQL. Other viewers see the collapsed comment.
  - Deleting an issue removes it from list, sync and search.
  - Three edits produce three history entries with correct editors.
  - Shape tests pass.

### P43 — Blocking enforcement, interaction limits, org moderation UI
- **Priority** 2 · **Wave** W4 · **Migrations** 5500–5599
- **Touches:** `bgh-core` (new `interaction.rs` with `can_interact(user, repo)`), write paths in `bgh-issues`, `bgh-pulls` and `bgh-repos` (fork, star, watch, collaborator add), `bgh-notify` (mention suppression), `bgh-accounts` (`interaction-limits` endpoints), web (org settings "Blocked users" and "Moderation", repo settings "Moderation", user settings "Blocked users")
- **Scope**
  - A user blocked by the repo owner (user or org) can't:
    - open or comment on issues and PRs, or react;
    - be added as a collaborator;
    - fork, star or watch;
    - trigger @mention notifications to the blocker.
    GitHub-shaped 403 or 422 messages.
  - Interaction limits:
    - `GET`/`PUT`/`DELETE /orgs/{org}/interaction-limits`, `/repos/{o}/{r}/interaction-limits` and `/user/interaction-limits`.
    - Values `existing_users`, `contributors_only` and `collaborators_only`, with `expiry` (one_day … six_months).
    - Enforced through the same helper.
  - UI: org blocked users (list, block, unblock), interaction-limit pickers, and a "Blocked users" settings page.
- **Acceptance**
  - A blocked user gets 403 on issue create, comment, react, fork and star in the blocker's repos.
  - A mention of the blocker by the blocked user doesn't notify.
  - `collaborators_only` blocks a non-collaborator's comment until expiry.
  - Shape tests pass.

### P44 — GraphQL org, repo and git surface (Terraform, Backstage, dashboards)
- **Priority** 2 · **Wave** W3 (after P19 and P23) · **Migrations** 5600–5699 (unused)
- **Touches:** `bgh-graphql` (`model/git.rs`, `actor.rs`, `repo.rs`, `query.rs` `node()`, new `mutation/branch_protection.rs` and `mutation/commits.rs`)
- **Scope**
  - Branch protection:
    - `Repository.branchProtectionRules` connection, `BranchProtectionRule` type with all fields Terraform reads, `Ref.branchProtectionRule`.
    - `create/update/deleteBranchProtectionRule` backed by the classic protection storage (pattern rules).
  - Rulesets: `Repository.rulesets` and `Organization.rulesets` (read).
  - Commits and trees:
    - `Commit.history(first, after, path, since, until, author)`, `associatedPullRequests`, `checkSuites`, `additions`/`deletions`/`changedFilesIfAvailable`, `file(path)`.
    - `Tree.entries`.
  - Org and teams:
    - `Organization.membersWithRole` (with role edges), `membersCanForkPrivateRepositories` and similar settings.
    - `Team.members` (membership filter and role), `repositories` (permission edges), `parentTeam`, `childTeams`, `privacy`.
  - `Repository.deployments` and `environments` (from P19/P20).
  - Mutations:
    - `createCommitOnBranch`: file additions and deletions, `expectedHeadOid`, signed by web-flow once P25 lands.
    - `updateLabel`, `deleteLabel`, `createLabel`.
    - `cloneTemplateRepository`.
  - `node()` resolves CheckRun, CheckSuite, Workflow, WorkflowRun, ReleaseAsset, PullRequestReviewComment, Reaction, DeployKey and BranchProtectionRule, with consistent `PullRequestReviewThread` ids between `/_bgh` and GraphQL.
- **Acceptance**
  - Query fixtures copied from the Terraform provider (`github_branch_protection` read, create and update) and Backstage `GithubOrgEntityProvider` succeed.
  - `createCommitOnBranch` creates a commit and rejects a stale `expectedHeadOid`.
  - `defaultBranchRef.target.history(first:10)` works.

### P45 — GraphQL timeline and review surface (VS Code PR extension)
- **Priority** 2 · **Wave** W4 (after P38, P41 and P42) · **Migrations** 5700–5799 (unused)
- **Touches:** `bgh-graphql` (`model/issue.rs`, `pull.rs`, `mutation/pulls.rs`, `mutation/issues.rs`, new `timeline.rs`), `bgh-issues/src/events.rs` (REST timeline additions)
- **Scope**
  - REST `/issues/{n}/timeline` for PRs adds `committed`, `reviewed` and `line-commented` items in GitHub shapes.
  - GraphQL `Issue.timelineItems` and `PullRequest.timelineItems`, with `itemTypes` filter and the item unions used by `gh` and VS Code: IssueComment, PullRequestCommit, PullRequestReview, Labeled/Unlabeled, Assigned, ClosedEvent, ReopenedEvent, CrossReferenced, ReviewRequested, HeadRefForcePushed, Merged, Renamed, ConnectedEvent, and so on.
  - Mutations:
    - `addReaction`, `removeReaction`.
    - `addPullRequestReviewThread`, `addPullRequestReviewComment`, `addPullRequestReviewThreadReply`.
    - `updatePullRequestReview`, `deletePullRequestReview`.
    - `updatePullRequestReviewComment`, `deletePullRequestReviewComment`.
    - `markFileAsViewed`, `unmarkFileAsViewed` (P38 storage).
    - `addSubIssue`, `removeSubIssue`, `reprioritizeSubIssue`.
    - `createIssue(parentIssueId)`.
  - Issue fields: `subIssues`, `parent`, `subIssuesSummary`, and real `trackedIssues` counts.
  - `PullRequestChangedFile.viewerViewedState`.
  - `viewerMergeHeadlineText` and `viewerMergeBodyText` computed from the repo settings.
- **Acceptance**
  - Recorded VS Code GitHub PR extension queries (timeline, add comment, add thread, mark viewed, reactions) validate and return data.
  - `gh pr view --comments` still works.
  - REST timeline shape tests pass.

### P46 — GitHub Apps, part 2: app webhooks, installation events, manifest flow, user-to-server tokens, checks attribution
- **Priority** 2 · **Wave** W3 (after P17) · **Migrations** 5800–5899
- **Touches:** `bgh-accounts/apps`, `bgh-notify` (per-app hook target, `/app/hook/*`, new payload file), `bgh-pulls/src/checks.rs` (app from token), web (app settings: advanced deliveries, manifest page)
- **Scope**
  - Per-app webhook:
    - Every installation's subscribed events go to the app hook URL with an `installation` object in the payload.
    - `GET`/`PATCH /app/hook/config`, `GET /app/hook/deliveries` and `/{id}`, and `POST /app/hook/deliveries/{id}/attempts`.
  - Events: `installation` (created/deleted/suspend/unsuspend/new_permissions_accepted) and `installation_repositories` (added/removed).
  - Permission-upgrade flow: an installation accepts new permissions.
  - Manifest flow: `POST /settings/apps/new` with `manifest` → redirect with `code` → `POST /app-manifests/{code}/conversions` returns id, PEM, webhook secret and client secret.
  - User-to-server tokens via the app OAuth (client id and secret, `ghu_`-style with refresh tokens). `/user/installations` works with them.
  - Check runs and suites created with installation tokens use the real app (`app` object, `app_id`). Remove the hardcoded fake apps except the built-in Actions app. `check_suite` is per app.
  - Required-check `app_id` matching (P3) uses real ids.
- **Acceptance**
  - A probot-style test app receives `installation` and `issues` events at its URL with a valid signature.
  - Manifest conversion returns credentials.
  - A check run created via installation token shows the app name and satisfies a check requiring that `app_id`.

### P47 — Fine-grained PATs, org token policies, narrow classic scopes
- **Priority** 2 · **Wave** W3 (after P17) · **Migrations** 5900–5999
- **Touches:** `bgh-accounts/src/tokens.rs`, `bgh-core` (`auth.rs`, `perms.rs`; reuses the P8/P17 route-category table), web (`/settings/tokens` fine-grained create flow, org settings "Personal access tokens")
- **Scope**
  - Fine-grained tokens:
    - Resource owner (user or org), repository selection (all, selected or public only), per-category permissions read/write, mandatory expiry (max per policy).
    - Distinct prefix.
    - Enforced like installation tokens.
  - Org policy:
    - Allow or deny fine-grained tokens, and require approval.
    - `GET /orgs/{org}/personal-access-token-requests` with approve/deny.
    - `GET /orgs/{org}/personal-access-tokens` and revoke.
    - Restrict classic PAT access to org resources.
    - Max lifetime.
  - Narrow classic scopes:
    - `repo:status` allows commit statuses on private repos.
    - `repo_deployment` allows deployments.
    - `public_repo` is honored.
    - `perms::effective` respects them per category instead of requiring full `repo`.
- **Acceptance**
  - A fine-grained token with selected repo A and `contents:read` clones A, can't push, and gets 404 on B.
  - An org requiring approval leaves the token pending until approved.
  - A `repo:status`-only token posts a status to a private repo and can't read its contents.
  - UI smoke.

### P48 — Custom repository roles, organization roles, member privileges, site-admin unlock
- **Priority** 2 · **Wave** W4 · **Migrations** 6000–6099
- **Touches:** `bgh-core/src/perms.rs` (fine-grained permission set behind the existing enum), `bgh-accounts` (`orgs.rs` member privileges, roles API), `bgh-repos` (collaborator and team role assignment, delete and visibility checks), `bgh-admin` (repo unlock), web (org settings Roles and Member privileges, site-admin unlock)
- **Scope**
  - Custom repository roles:
    - `/orgs/{org}/custom-repository-roles` CRUD: base role read/triage/write/maintain plus extra fine-grained permissions (GitHub's permission list subset).
    - Assignable to collaborators and teams.
  - Organization roles: `/orgs/{org}/organization-roles` with the pre-defined all-repo read/triage/write/maintain/admin and security manager roles, assigned to users and teams.
  - Member privileges, each enforced:
    - `members_can_delete_repositories`
    - `members_can_change_repo_visibility`
    - `members_can_invite_outside_collaborators`
    - `members_can_delete_issues`
    - `members_can_create_pages`
    - `members_can_fork_private_repositories` (exists)
  - Site admins no longer implicitly have admin on private repos:
    - Read-only metadata by default.
    - "Unlock repository" with a reason, time-limited (default 2h) and audited as `staff.repo_unlock`.
    - Config flag for the legacy behavior.
- **Acceptance**
  - A custom role "write plus manage labels" works.
  - A security-manager team can read all repos.
  - A member with `members_can_delete_repositories=false` gets 403 on delete.
  - A site admin gets 404 on private content until unlock; the unlock is audited and expires.
  - Shape tests pass.

### P49 — SAML SSO and SCIM provisioning
- **Priority** 2 · **Wave** W4 (after P14) · **Migrations** 6100–6199
- **Touches:** `bgh-accounts` (new `saml/` and `scim/`), `bgh-core` settings (auth providers), `Dockerfile` (xmlsec deps if using `samael`), web admin settings, login page
- **Scope**
  - SAML 2.0 SP:
    - Metadata endpoint, SP-initiated login, ACS (signed assertions required), optional encrypted assertions.
    - Attribute mapping (username, name, emails, ssh keys, admin and groups), JIT provisioning, linked identities, single logout optional.
    - Group attribute feeds P14's `external_group_mappings`.
  - SCIM 2.0:
    - `/scim/v2/enterprises/{enterprise}/Users` and `Groups` (GHES shape) plus `/scim/v2/organizations/{org}/Users`.
    - Create, patch, deactivate (suspend plus revoke sessions, tokens and keys), Groups → team sync.
    - Bearer token auth with a dedicated admin-issued token scope.
  - Deprovisioning on IdP removal revokes all credentials immediately.
- **Acceptance**
  - SAML round-trip test against a test IdP (signed fixtures, or an in-process signer).
  - SCIM conformance tests: create, get, filter by `userName eq`, PATCH active=false suspends and revokes PATs, Groups patch syncs a team.
  - Admin UI smoke.

### P50 — Account and repo lifecycle: self-service rename with redirects, deletion, restore, transfers to users
- **Priority** 2 · **Wave** W3 · **Migrations** 6200–6299
- **Touches:** `bgh-accounts` (`users.rs` PATCH login and `DELETE /user`; `orgs.rs` `DELETE /orgs/{org}` and rename), `bgh-admin/src/service.rs` (`rename_account` redirects), `bgh-repos` (`repos.rs` soft delete and restore, `settings.rs` transfer to user with acceptance), `bgh-core` perms `RepoAccess::load` redirect, web (account settings, org settings danger zone, `/settings/repositories/deleted`, transfer acceptance)
- **Scope**
  - Renaming a user or org (self-service where GitHub allows: the user, and the org owner for orgs) inserts `repo_redirects` for every owned repo and an owner-level redirect. Old git remotes and URLs keep working. Rate-limited, and the old login is reserved for 90 days.
  - `DELETE /user` (sudo, P36) and `DELETE /orgs/{org}` (owner):
    - Content is attributed to `ghost`.
    - Owned repos are deleted (soft).
    - Sole-owner checks apply.
  - Repo soft delete:
    - `deleted_at`, storage kept 90 days.
    - Restore via `POST /_bgh/repos/{id}/restore` (owner) and from admin.
    - Purge job after retention. Name freed immediately; restore fails if the name is taken.
  - Transfer to another user creates a pending transfer that the recipient accepts by email or UI link; it expires after 1 day. Org transfers keep the current flow.
- **Acceptance**
  - Rename user alice→alice2: `git fetch` on the old remote works and the API redirects (301).
  - Delete and then restore a repo returns all its data.
  - A user transfer completes only after acceptance.
  - Deleting an account shows ghost on old comments.

### P51 — Metadata importer, part 2: pull requests, reviews, wiki, GitLab source, mannequin reclaim
- **Priority** 2 · **Wave** W3 (after P18) · **Migrations** 6300–6399
- **Touches:** `bgh-import`, `bgh-pulls` (internal insert API with explicit numbers, states and refs), `bgh-wiki` (git import), web admin and org import pages
- **Scope**
  - PRs with original numbers:
    - Fetch `refs/pull/*/head` from the source.
    - Recreate base and head refs (closed PRs whose branches are gone keep `refs/pull/N/head`).
    - Preserve state, merged commit, merge time, draft.
  - Reviews and review comments: path, line, side, start_line, original_commit, in_reply_to. Outdated mapping via the original commit.
  - Requested reviewers.
  - Wiki: fetch `{repo}.wiki.git`.
  - Webhooks configs (disabled after import), branch protection and rulesets.
  - GitLab source: issues, merge requests, notes, labels, milestones via the GitLab REST API, mapped to issues and PRs.
  - Mannequin reclaim: an org owner maps a mannequin to a real user (attribution rewrite), with the invitee accepting.
- **Acceptance**
  - A fake GitHub API fixture with merged, closed and open PRs, reviews and threaded comments imports with numbers, state and line positions intact. Reviews render in the UI.
  - A GitLab fixture imports MRs as PRs.
  - Reclaiming a mannequin moves attribution.

### P52 — Issue page sidebar: Subscribe, Projects section, org `.github` template fallback
- **Priority** 2 · **Wave** W4 · **Migrations** 6400–6499 (unused)
- **Touches:** web (`pages/issues/IssueSidebar.tsx`, the PR sidebar, `NewIssuePage.tsx`), `bgh-issues/src/templates.rs` (org fallback), PR template lookup in `web/src/api/endpoints.ts` (shared helper only; P55 owns the ComparePage UI)
- **Scope**
  - Subscribe/Unsubscribe button with reason text on issues and PRs (the existing `/_bgh/…/subscription` endpoints), plus a custom-events menu.
  - Projects section:
    - Add or remove the issue in projects (P13 API or the existing `/_bgh`).
    - Show and edit the project field values (status, iteration, number, …) inline.
  - On create, apply the template `projects:` and `type:` (types from P41).
  - `project:owner/number` filter in the issue list.
  - Issue templates, `config.yml` and PR templates fall back to the owner's `.github` repository when the repo has none.
- **Acceptance**
  - Subscribe toggles and survives reload.
  - Adding to a project from the sidebar sets the status field.
  - A template with `projects: ["org/1"]` adds the new issue.
  - A repo without templates shows the org `.github` templates (backend test).

### P53 — Issue and PR search qualifier parity (client filters and server search)
- **Priority** 2 · **Wave** W4 · **Migrations** 6500–6599 (indexes)
- **Touches:** `web/src/pages/issues/filters.ts`, `bgh-search/src/issues.rs` (also used by GraphQL search)
- **Scope**
  - Shared qualifier grammar:
    - Support `mentions:`, `involves:`, `commenter:`, `reason:`, `is:locked`, `linked:pr|issue`, `created:`/`updated:`/`closed:`/`merged:` ranges, `comments:>n`, `reactions:`, `interactions:`.
    - Negation for `-author:`, `-assignee:`, `-label:`, …
    - `label:a,b` (OR), `type:`, `project:`/`no:project`, `parent-issue:`, `has:sub-issues`.
    - `review:none|required|approved|changes_requested`, `status:success|failure|pending`.
    - `team-review-requested:`, `user-review-requested:`, `review-requested:`, `team:`.
    - Boolean AND/OR with parentheses (GitHub advanced search).
    - `in:title,body,comments`, `sort:reactions-+1` and friends.
  - Client list:
    - Evaluate locally when every qualifier is supported locally; otherwise fall back to the server `/search/issues` with a "searching server" state.
    - Free text matches title and body locally.
  - Server: implement the missing qualifiers, and stop ignoring unknown ones (match GitHub's behavior of treating them as text).
- **Acceptance**
  - A table-driven test corpus of about 60 queries returns the same result sets locally and on the server.
  - `gh pr list --search "review:required status:failure"` returns the correct PRs.
  - `no:project` filters correctly.

### P54 — Timeline performance and composer UX
- **Priority** 2 · **Wave** W4 · **Migrations** 6600–6699 (saved replies)
- **Touches:** `web/src/pages/issues/Timeline.tsx` (owner in W4), `MarkdownEditor.tsx`, `ReviewThread.tsx` (drafts only), `bgh-accounts` (`/_bgh/user/saved_replies`), web settings
- **Scope**
  - Timeline:
    - Memoize `buildItems` (computed).
    - Collapse long threads: first 10 and last 20 items shown, then "N hidden items — Load more".
    - Virtualize when more than 200 items.
    - Render Markdown lazily off-screen.
  - Drafts: per-issue and per-PR comment and review drafts persist in IndexedDB and localStorage, survive navigation and reload, and clear on submit.
  - Comment menu: Quote reply (also the `r` shortcut on a selection), "Reference in new issue", and "Report content" (to admin moderation, stored).
  - Hovercards for `#N` references and @mentions, using sync data first.
  - Saved replies: settings CRUD and a picker in the editor toolbar (Ctrl+.).
- **Acceptance**
  - A 2,000-comment issue renders in under 300 ms (perf test with a mock fixture) and scrolls smoothly.
  - A draft survives navigation and reload.
  - Quote reply inserts a quoted selection.
  - Saved replies work.
  - Vitest coverage.

### P55 — PR creation form
- **Priority** 2 · **Wave** W4 · **Migrations** 6700–6799 (unused)
- **Touches:** `web/src/pages/pulls/ComparePage.tsx`, `web/src/api/endpoints.ts` template discovery
- **Scope**
  - Sidebar for reviewers, assignees, labels, projects and milestone, applied after creation in one batch.
  - Full `MarkdownEditor` with preview, mentions and attachments (P6).
  - "Allow edits by maintainers" checkbox (`maintainer_can_modify`).
  - Templates:
    - Multiple templates from `.github/PULL_REQUEST_TEMPLATE/*.md` with `?template=` and a picker.
    - Locations checked in parallel, with the org `.github` fallback.
  - URL parameters `?title=`, `?body=`, `?labels=`, `?assignees=`, `?milestone=`, `?projects=`, `?expand=1`, `?quick_pull=1`.
  - Draft vs. ready split button kept.
- **Acceptance**
  - `/o/r/compare/main...feat?expand=1&title=x&labels=bug&template=feature.md` pre-fills everything.
  - Creating with reviewers and labels applies them.
  - Vitest for parameter parsing.

### P56 — Discussions backend (REST-less, GraphQL API, webhooks)
- **Priority** 2 · **Wave** W4 · **Migrations** 6800–6899
- **Touches:** new crate `bgh-discussions`, `bgh-graphql` (new model and mutation files), a payload file in `bgh-notify`, `bgh-search` (`SearchType::Discussion`), `bgh-issues` (convert issue to discussion), `bgh-releases` (`discussion_category_name`)
- **Scope**
  - Schema:
    - Categories: name, emoji, description, format open/Q&A/announcement/poll; default categories created when `has_discussions` is enabled.
    - Discussions with numbers in the shared issue/PR number sequence (GitHub shares it).
    - Threaded comments (one level of replies).
    - Answers for Q&A, upvotes, reactions, lock, pin, labels, close with reason.
    - Polls as a stretch goal.
  - GraphQL:
    - `Repository.discussions`, `discussion(number)`, `discussionCategories`.
    - Mutations `createDiscussion`, `updateDiscussion`, `deleteDiscussion`, `addDiscussionComment`, `updateDiscussionComment`, `deleteDiscussionComment`, `markDiscussionCommentAsAnswer`/`unmark…`, `addUpvote`/`removeUpvote`, `closeDiscussion`/`reopenDiscussion`, `lock…`.
  - Webhooks `discussion` and `discussion_comment`.
  - Notifications, search, sync shape (list, local-first), activity.
  - Convert issue to discussion. A release creates a discussion in the named category.
- **Acceptance**
  - GraphQL fixtures from GitHub docs examples run.
  - Q&A answer flow.
  - Webhooks delivered.
  - `gh api graphql` discussion queries work.
  - Search returns discussions.

### P57 — Discussions web UI
- **Priority** 2 · **Wave** W5 · **Migrations** 6900–6999 (unused)
- **Touches:** web (new `pages/discussions/*`, RepoLayout Discussions tab when enabled, settings toggle wiring)
- **Scope**
  - Discussions list with category sidebar, filters, sort, answered and unanswered, and pinned.
  - Discussion page: threaded comments, answer marking, upvotes, reactions, lock, pin, close.
  - New discussion form with category templates (`.github/DISCUSSION_TEMPLATE/*.yml`).
  - Category management page.
  - "Convert to discussion" action on issues.
- **Acceptance**
  - Smoke flow: create a Q&A discussion, reply, mark answered, upvote.
  - The tab is hidden when the feature is disabled.
  - Mock backend coverage.
  - Vitest.

### P58 — Projects governance and automation
- **Priority** 2 · **Wave** W4 · **Migrations** 7000–7099
- **Touches:** `bgh-projects` (`access.rs`, workflows, fields, convert, copy, status updates, events), a payload file in `bgh-notify`, `bgh-graphql` (new mutations in P13's files: `convertProjectV2DraftIssueItemToIssue`, `copyProjectV2`, `createProjectV2StatusUpdate`), web `pages/projects` (`SettingsDialog`, `ItemPanel`)
- **Scope**
  - Per-project roles:
    - Collaborators: users and teams as read/write/admin.
    - Org base project permission (none/read/write); stop giving every member write.
    - Private user projects can be shared.
  - Convert a draft to an issue (choose repo, keep field values).
  - Workflows:
    - Add code_changes_requested, code_review_approved, pr_linked_to_issue and auto-close issue when Status=Done.
    - Implement all workflows in the mock backend too.
  - Built-in fields: Linked pull requests, Reviewers, Parent issue, Sub-issue progress, Issue type.
  - Project templates, copy project, status updates (on track / at risk / off track with body and dates).
  - Events and webhooks `projects_v2`, `projects_v2_item`, `projects_v2_status_update`. Audit entries.
- **Acceptance**
  - A private org project hides from a non-collaborator member.
  - Converting a draft creates an issue linked to the item.
  - The auto-close workflow closes the issue.
  - Webhooks delivered.
  - A copy keeps fields and views.

### P59 — Projects views and insights
- **Priority** 2 · **Wave** W5 · **Migrations** 7100–7199 (item history)
- **Touches:** `bgh-projects` (item field history recording and an insights query API, view options), web (`pages/projects`: new `InsightsView`, `RoadmapView` rewrite, `BoardView`)
- **Scope**
  - Record item field changes in history.
  - Insights: burn-up chart, plus current and historical charts grouped and sliced by any field; save chart configs. Lazy chart chunk, following dataviz guidance.
  - Roadmap:
    - Separate start and target date fields or an iteration.
    - Drag to move, drag edges to resize.
    - Zoom month, quarter and year; markers for milestones and iterations.
  - Board:
    - Horizontal group-by (swimlanes).
    - Column item limits.
    - Field sums per column and group.
    - Slice-by panel.
    - Virtualized columns.
  - Table: bulk multi-select edits and copy/paste of cells.
- **Acceptance**
  - The burn-up chart matches the history fixture.
  - Roadmap drag updates the dates (optimistic, persisted).
  - A board with 1,000 cards stays smooth.
  - Sums are correct.
  - Vitest.

### P60 — Inbox features, auto-watch, missing notification triggers
- **Priority** 2 · **Wave** W4 · **Migrations** 7200–7299
- **Touches:** `bgh-notify` (`threads.rs` saved and done views, `fanout.rs`, settings), `bgh-repos` and `bgh-accounts` (auto-watch hooks on access and team join), web `pages/notifications`, settings notifications
- **Scope**
  - Inbox, Saved and Done views:
    - `saved` field in the sync model.
    - Done threads are queryable and can be moved back to the inbox.
    - Undo toast for done, unsubscribe and read.
  - Inbox query box: `repo:`, `org:`, `author:`, `reason:`, `is:unread|read|done|saved`, `is:issue-or-pull-request`, `is:release`, free text.
  - Custom views stored server-side.
  - Thread subscription state comes from the sync store (no per-preview fetch).
  - Auto-watch:
    - User preferences "Automatically watch repositories" and "Automatically watch teams", default on.
    - Watch rows are created on gaining push access or joining a team.
  - Triggers:
    - New mentions added by comment edits (issue comment and review comment edits).
    - Invitation notifications for repo and org invites.
    - Review dismissed → notify the review author.
    - Add `invitation` to the web reason labels.
- **Acceptance**
  - Done → Inbox restores the thread.
  - Saving pins it in the Saved view.
  - A user added as a collaborator gets notifications for new issues.
  - An edit that adds an @mention notifies.
  - Query parsing tests.

### P61 — Observability: Prometheus metrics, JSON logs, request context
- **Priority** 2 · **Wave** W3 · **Migrations** 7300–7399 (unused)
- **Touches:** `bgh-server` (`/metrics`, logging init, request span), workspace `Cargo.toml` (`tracing-subscriber` json feature, `metrics` and `metrics-exporter-prometheus`, optional `opentelemetry-otlp`), small additive instrumentation in `bgh-core` (jobs, events lag from P9, db pool), `bgh-git` (op durations), `bgh-sync` (ws connections)
- **Scope**
  - `/metrics`: Prometheus text, protected by a bearer token setting or a listen address, never public by default. Metrics:
    - HTTP request count and latency histograms by route template and status.
    - Rate-limit rejections.
    - Job queue depth and failures by kind.
    - Event-bus consumer lag.
    - DB pool in-use and idle.
    - Redis errors.
    - Git operation durations by kind (upload-pack, receive-pack, archive, merge-tree).
    - Sync WebSocket connections.
    - Webhook delivery success and failure.
    - Actions queue length.
  - `BGH_LOG_FORMAT=json|pretty`.
  - The request span adds `user_id`, `token_id`, `auth_method` and client IP (trusted-proxy aware).
  - Optional OTLP traces export (`BGH_OTLP_ENDPOINT`).
- **Acceptance**
  - Scraping `/metrics` shows the metric families after a smoke run.
  - JSON log lines parse and include user and request ids.
  - Unauthenticated `/metrics` returns 401 when a token is set.

### P62 — Backup and restore CLI, admin CLI, upgrade preflight
- **Priority** 2 · **Wave** W4 · **Migrations** 7400–7499 (unused)
- **Touches:** `bgh-server/src/main.rs` (CLI subcommands), `bgh-core/src/db.rs` (migration status), docs section stub (P77 consolidates)
- **Scope**
  - `bgh backup --to DIR`:
    - Consistent `pg_dump`.
    - Hardlink-incremental rsync-style snapshot of repos, LFS, release assets, packages, attachments, SSH host key, Actions `server.key` and signing keys.
    - Excludes caches.
    - Writes a manifest.
  - `bgh restore --from DIR` with a version check and a post-restore `git fsck --connectivity-only` sample.
  - `bgh backup verify`.
  - Admin commands, each operating directly on the DB:
    - `bgh admin suspend|unsuspend|promote|demote|reset-password|disable-2fa <user>`
    - `bgh admin maintenance on|off [--message]`
    - `bgh admin repo fsck|repack <o/r>`
    - `bgh admin reindex search [--repo]`
    - `bgh admin settings export|import`
  - `bgh migrate --status|--dry-run`.
  - `bgh preflight`: git version, disk space, PG version, pending migrations, writable data dir.
  - An old binary refuses to start when the DB has unknown migrations, with a clear message.
- **Acceptance**
  - Backup, then wipe, then restore on a test instance reproduces repos, issues and Actions secrets (decryptable).
  - Incremental backup reuses unchanged files (hardlink count).
  - `disable-2fa` recovers a locked-out admin.
  - Preflight output tested.

### P63 — Admin ops polish
- **Priority** 2 · **Wave** W4 · **Migrations** 7500–7599 (unused)
- **Touches:** `bgh-core/src/settings.rs` (maintenance exemptions, `webhooks` section, SSH listen), `bgh-repos` (`ssh/mod.rs` listen address and maintenance gate; download routes), `bgh-admin` (test email, global hook deliveries, reindex, user edits, reports), `bgh-notify` (deliveries for global hooks), web site-admin pages
- **Scope**
  - Maintenance mode:
    - Exempt `/_bgh/sso/*` login and callback.
    - Gate git over SSH and GET downloads (raw, archive, LFS) for non-admins.
    - The CLI toggle is in P62.
  - `BGH_SSH_LISTEN` (default `0.0.0.0:<ssh_port>`) is decoupled from `BGH_LISTEN`.
  - `webhooks` settings section: `allowed_hosts` and timeout, editable in the admin UI (read by `ssrf.rs`).
  - "Send test email" endpoint and button.
  - Global hook deliveries: list, detail and redeliver at `/admin/hooks/{id}/deliveries…` plus UI.
  - Code search reindex for one repo or all repos (API and UI).
  - Admin user edits:
    - Email and name.
    - Revoke an individual PAT or SSH key.
    - Bulk suspend of dormant users.
  - CSV reports: all, dormant and suspended users; active repos.
  - Policies: session TTL and reserved logins.
- **Acceptance**
  - An SSO-only admin can sign in during maintenance.
  - SSH push is refused during maintenance.
  - SSH listens on 0.0.0.0 while HTTP is on 127.0.0.1.
  - A webhook to an allow-listed 10.x host succeeds after a UI edit.
  - Test email lands in the mail sink.
  - Global hook redelivery works.

### P64 — Audit log completeness, streaming and the user security log
- **Priority** 2 · **Wave** W5 · **Migrations** 7600–7699
- **Touches:** `bgh-core/src/audit.rs` (extra columns), `bgh-repos` (git events), `bgh-actions` (secrets, variables, environments and runners audit), `bgh-admin` (streaming config and export), web (site-admin and org audit pages, `/settings/security-log`)
- **Scope**
  - New audit columns: `token_id`, `programmatic_access_type` (PAT, OAuth, app, job, impersonation), `impersonator_id`, `user_agent`, `request_id`.
  - Git events `git.clone`, `git.fetch`, `git.push` (push always; clone and fetch configurable and sampled), served by `include=git`.
  - Audit all Actions secret, variable, environment, runner and policy changes.
  - Streaming to HTTP (HMAC-signed), S3-compatible, Splunk HEC and Datadog:
    - Retried through jobs.
    - Health status.
    - Admin UI to configure and test.
  - Server-side export (JSON and CSV, async job, download link).
  - `/settings/security-log`: the user's own login, 2FA, token, key and app events.
- **Acceptance**
  - A push via impersonation token is recorded with `impersonator_id` and token type.
  - `include=git` returns pushes.
  - The HTTP stream receives events with a valid signature.
  - Export of 50k rows completes.
  - A user sees their own security log only.

### P65 — Secret scanning and push protection
- **Priority** 2 · **Wave** W3 · **Migrations** 7700–7799
- **Touches:** new crate `bgh-security` (pattern engine, alerts), `bgh-git` pre-receive hook (via the P2/P23 object enumeration), `bgh-repos` settings (`security_and_analysis` honored), web (repo Security → Secret scanning alerts, push-protection bypass page)
- **Scope**
  - Pattern set:
    - High-confidence provider patterns: GitHub-style tokens including this server's own token prefixes, AWS keys, GCP, Azure, Slack, Stripe, private keys, npm, PyPI, …
    - Custom patterns per repo and org (regex, test strings).
  - Push protection:
    - Scan new blobs in pre-receive and reject with a GitHub-like message that includes a bypass URL.
    - Bypass reasons (used in tests, false positive, will fix later) create alerts.
  - Background scans of history on enable and of new pushes; alert dedupe by secret hash.
  - API:
    - `/repos/{o}/{r}/secret-scanning/alerts` (+ `{n}` PATCH resolve with reason, `/locations`), and org-level list.
    - `security_and_analysis.secret_scanning(.push_protection)` toggles.
  - Webhook `secret_scanning_alert`.
  - Admin site-wide enablement.
- **Acceptance**
  - Pushing a file with an AWS key is blocked. After a bypass, the push passes and an alert exists.
  - A history scan finds a planted key.
  - Resolving the alert via the API works.
  - Custom pattern test.
  - Scan throughput benchmark on a 100 MB push.

### P66 — Code scanning (SARIF) and the Security tab
- **Priority** 2 · **Wave** W4 (after P65) · **Migrations** 7800–7899
- **Touches:** `bgh-security` (SARIF ingestion and alerts), `bgh-pulls` (PR alert annotations as a check run), `bgh-graphql` (`securityPolicyUrl`, `isSecurityPolicyEnabled`), web (repo Security tab: overview, policy, code scanning, secret scanning)
- **Scope**
  - `POST /repos/{o}/{r}/code-scanning/sarifs` (gzip+base64 SARIF; 202 with id, then `GET /sarifs/{id}`).
  - `/code-scanning/analyses[/{id}]`, `/code-scanning/alerts[/{n}]` (PATCH dismiss with reason, `/instances`).
  - Alert lifecycle across commits and branches, matched by fingerprint and rule.
  - PR integration: a "Code scanning results" check run with annotations for new alerts on the PR diff.
  - The `code_scanning` ruleset rule (block merge on severity threshold) via the P3 evaluator.
  - Webhook `code_scanning_alert`.
  - Security tab replaces the placeholder:
    - SECURITY.md policy (repo, `.github/` or `docs/`, plus the org `.github` fallback).
    - Code scanning alerts list and detail with source context.
    - Secret scanning (P65).
  - `codeql-action/upload-sarif` compatibility: honor `ref`, `commit_sha`, `checkout_uri`, `tool_name`.
- **Acceptance**
  - A Semgrep or Trivy SARIF fixture upload creates alerts, and re-uploading on a fixed commit closes them.
  - A PR with a new alert gets annotations.
  - The ruleset blocks the merge.
  - `securityPolicyUrl` is set when SECURITY.md exists.
  - UI smoke.

### P67 — Sync bootstrap scaling
- **Priority** 2 · **Wave** W5 · **Migrations** 7900–7999 (indexes)
- **Touches:** `bgh-sync` (`bootstrap.rs`, paging), `docs/SYNC_PROTOCOL.md`, `web/src/sync` (`schema.ts`, `client.ts`, `persistence.ts`), `web/src/app/Shell.tsx` gating
- **Scope**
  - Bootstrap open issues and PRs plus recently closed ones (configurable, for example closed within 90 days) by default. Older closed issues load lazily per repo and per query (partial sync).
  - Per-repo incremental bootstrap: the shell renders after metadata plus the current route's scope, and the rest streams in the background with progress.
  - Cap the default scope set by recency of access, and load others on demand.
  - `navigator.storage.persist()`.
  - LRU eviction for lazily loaded rows (`loadedIssues`), with a quota-exceeded handler.
  - Update the protocol doc and versioning, with backward-compatible client handling.
- **Acceptance**
  - An org fixture with 300 repos and 50k issues: time to first render under 1.5 s on a warm server (measured in a scripted benchmark), with steady memory.
  - Old closed issues still open via deep link and search.
  - Protocol tests in `bgh-sync`.

### P68 — Web performance polish
- **Priority** 2 · **Wave** W5 · **Migrations** 8000–8099 (unused)
- **Touches:** `web/src/api/cache.ts`, `web/src/sw.ts`, `web/vite.config.ts`, `web/build/plugins.ts`, `web/src/boot.ts`, `web/src/router/index.tsx` (reload guard only), `routes.ts` (prefetch keys), list pages (`DirView`, `BoardView`, Labels, Milestones, Releases, wiki pages list)
- **Scope**
  - Persist immutable SHA-keyed REST cache entries (trees and blobs by sha, commits) and last-seen lists to IndexedDB with LRU. Serve stale-while-revalidate across reloads.
  - Service worker: cache immutable `/_bgh` blob and tree responses by sha.
  - Build:
    - Group octicons into one `icons` chunk.
    - Exclude every `src/mock/**` chunk from the precache.
    - `?mock` only works in dev builds.
  - Chunk-load failure reloads once (sessionStorage guard), then shows an error page.
  - Add `prefetch` to repo settings, user and org settings, site-admin, compare, search, wiki edit and history, and release new.
  - Virtualize the listed pages.
- **Acceptance**
  - After a reload, a previously viewed blob renders with no network wait (devtools offline test).
  - The `dist/sw.js` precache has no mock chunks.
  - JS chunk count drops by ≥60, and the size budget holds.
  - A forced chunk failure doesn't loop.
  - A directory with 10k entries scrolls smoothly.

### P69 — Mobile layout and accessibility
- **Priority** 2 · **Wave** W5 · **Migrations** 8100–8199 (unused)
- **Touches:** `web/src/app/Shell.module.css` and Shell, `pages/repo`, `pages/pulls`, `components/diff`, `pages/issues` CSS, `web/src/router/index.tsx` (focus and announce)
- **Scope**
  - Breakpoints at ≤768px and ≤480px:
    - The sidebar becomes an off-canvas drawer.
    - The repo header wraps and the nav tabs scroll horizontally.
    - The PR detail sidebar moves below the content.
    - Diffs are forced to unified view with horizontal scroll per file.
    - Issue list rows are compact.
    - Touch-size targets.
  - Route changes:
    - Move focus to the main `h1` (or main region).
    - Announce the page title through an `aria-live="polite"` region.
  - Audit key flows with axe: focus-visible, label coverage, and dialogs trapping focus.
- **Acceptance**
  - Playwright or manual screenshots at 375px for inbox, issue, PR conversation and diff: usable, no horizontal page scroll.
  - Axe has no serious violations on 6 core pages.
  - A screen reader announces navigation (manual check note).

### P70 — Review routing: CODEOWNERS validation, code-owner indicators, team review assignment
- **Priority** 2 · **Wave** W5 · **Migrations** 8200–8299
- **Touches:** `bgh-pulls` (`codeowners.rs`, `reviewers.rs`), `bgh-repos` (`/codeowners/errors` route), `bgh-accounts` (team review settings), web (`FileView` CODEOWNERS annotations, `Reviewers.tsx`, team settings)
- **Scope**
  - `GET /repos/{o}/{r}/codeowners/errors?ref=`: syntax errors, unknown users or teams, owners without write, in GitHub's error shape.
  - The blob view of CODEOWNERS highlights invalid lines with messages.
  - The reviewers sidebar marks code-owner requests ("Code owner" badge from `as_code_owner`). Teams get a re-request button.
  - Team code-review settings:
    - Auto-assignment enabled, count, algorithm round robin or load balance.
    - Exclude members, skip members with busy status (once P76/profile status lands; otherwise ignore).
    - Optionally remove the team request after assignment.
  - Notify only the assigned members.
- **Acceptance**
  - A CODEOWNERS file with a typo team shows errors in the API and UI.
  - Requesting a team with round robin 2 assigns two members and rotates on the next PR.
  - The badge shows.

### P71 — SSH and git transport: SSH CAs, upload-archive, RSA host key, push options, LFS Range
- **Priority** 2 · **Wave** W5 · **Migrations** 8300–8399
- **Touches:** `bgh-repos/src/ssh` (cert auth, upload-archive, host keys), `bgh-accounts` (org SSH CA API), `bgh-git/src/smart_http.rs` (push options), `bgh-core` events (additive `push_options` on `PushEvent`), `bgh-repos/src/lfs` (Range)
- **Scope**
  - Org SSH certificate authorities:
    - `/orgs/{org}/ssh-certificate-authorities` CRUD.
    - OpenSSH user certificates (the `login@<host>` extension or principals mapping) authenticate as the user for that org's repos.
    - Optional "require SSH certificates".
  - `git-upload-archive` over SSH (`git archive --remote`).
  - Add an RSA host key (and ECDSA) alongside Ed25519, published in `/meta`.
  - Push options:
    - Read `push-option` values, and pass them in `PushEvent`, the webhook payload (where GitHub does) and Actions context.
    - Support `ci.skip` / `[skip ci]` semantics for Actions.
  - LFS downloads honor `Range`.
  - SSH-native `git-lfs-transfer` as a stretch goal.
- **Acceptance**
  - Clone with a CA-signed certificate works, and an unknown CA is rejected.
  - `git archive --remote=ssh://…` works.
  - An RSA-only client connects.
  - `git push -o ci.skip` skips workflows.
  - An LFS Range request returns 206.

### P72 — Activity and Atom feeds
- **Priority** 2 · **Wave** W5 · **Migrations** 8400–8499 (feed tokens)
- **Touches:** `bgh-search/src/activity` (`record.rs` gaps and new feed renderers), `bgh-server` (HTML-host `.atom` routes), `bgh-accounts` (feed token)
- **Scope**
  - Atom feeds:
    - `/{user}.atom`, `/{o}/{r}/releases.atom`, `/{o}/{r}/tags.atom`, `/{o}/{r}/commits/{branch}.atom`.
    - Private dashboard feed `/{user}.private.atom?token=` (per-user feed token, revocable).
    - Org feed.
  - `GET /feeds` per GitHub's shape, with `_links`.
  - Events API gaps:
    - `IssuesEvent` actions assigned, unassigned, labeled and unlabeled.
    - `GollumEvent` (from P10's emits).
    - `MemberEvent` added and removed.
    - `PublicEvent` (fixed by P10).
    - Org membership events in org feeds.
    - `CommitCommentEvent` (if P32 merged).
- **Acceptance**
  - Feeds validate as Atom (parser test) and respect privacy: a private repo appears only in the token feed.
  - `/feeds` shape test.
  - The new event types appear in `/users/{u}/events`.

### P73 — Email: reply-by-email, per-org routing, reason Cc, digests, encrypted webhook secrets
- **Priority** 2 · **Wave** W5 · **Migrations** 8500–8599
- **Touches:** `bgh-notify` (`email.rs`, settings, new inbound module, webhook secret storage), `bgh-admin/src/hooks.rs` (secret encryption), `bgh-core` mail, web notification settings
- **Scope**
  - Reply-by-email:
    - Outgoing `Reply-To: reply+<signed token>@<reply domain>`.
    - Inbound via a provider webhook (Mailgun, SendGrid, Postmark style, generic MIME POST with a shared secret) and an optional built-in LMTP listener.
    - Strip quoted text and signatures.
    - Post as an issue, PR or review comment from the verified sender, with authorization rechecked.
  - Custom routing: a verified email per org.
  - `Cc: <reason>@noreply.<host>` addresses, as GitHub does.
  - Daily digest option per reason.
  - Webhook secrets (repo, org, global, app) encrypted at rest with a migration of existing rows.
- **Acceptance**
  - A posted MIME reply fixture creates a comment by the right user; a tampered token is rejected.
  - Routing sends org-A mail to the routed address.
  - The digest batches.
  - The DB has no plaintext webhook secrets, and signatures still verify.

### P74 — Polish: Pull requests web
- **Priority** 2 · **Wave** W5 · **Migrations** 8600–8699 (unused)
- **Touches:** `web/src/pages/pulls/*`, `MergeBox.tsx` (owner in W5), `Timeline.tsx` review and event items (owner in W5), `ChecksTab.tsx`, the router hash-scroll helper, sync check-run shape (additive output fields)
- **Scope**
  - Review controls:
    - Dismiss review UI (with message; respects P3 restrictions).
    - Edit review summary (`PUT reviews/{id}`).
  - Header: change base branch next to the title (`setPullBase`, with confirm).
  - Timeline rendering:
    - Team reviewers render correctly.
    - `review_dismissed` with its message.
    - `base_ref_changed`, `head_ref_deleted`/`restored`, `auto_merge_enabled`/`disabled`.
    - Force-push shows before→after with a compare link.
    - Commits pushed between reviews shown inline.
  - Deep links: `#issuecomment-`, `#discussion_r` and `#pullrequestreview-` scroll after sync loads (shared router helper; expand resolved threads). Add `id` on review items.
  - Merge box:
    - Stop overriding the repo commit-message settings: pre-fill from `viewerMergeHeadlineText`/`viewerMergeBodyText` and send the edited text exactly, including empty values.
    - Merge title uses `owner/branch`.
    - Head-branch state synced (`head_ref_exists`): "Restore branch", and delete or restore for closed PRs.
    - Hide auto-merge when `allow_auto_merge` is false.
  - Checks tab: render `output.summary` and `output.text` markdown and requested-action buttons (`POST requested_action`).
- **Acceptance**
  - Vitest for message defaults and hash scrolling.
  - Smoke: dismiss a review, change the base, restore a branch.
  - Merge with the repo setting `PR_TITLE` produces the right title.
  - A check summary renders.

### P75 — Polish: Actions
- **Priority** 2 · **Wave** W5 · **Migrations** 8700–8799 (unused)
- **Touches:** `bgh-actions` (`runner/checkout.rs`, `runner/artifacts.rs`, `api/runs.rs`, `services.rs`), web `pages/actions`
- **Scope**
  - `actions/checkout` native inputs: `submodules` (true/recursive), `lfs`, `sparse-checkout` (and `-cone-mode`), `fetch-tags`, `persist-credentials`, `filter`, `show-progress`.
  - `download-artifact` with `run-id` and `github-token` (cross-run). `upload-artifact` with `overwrite` and `compression-level`.
  - `GET /actions/runs/{id}/timing`. Rerun with `enable_debug_logging` (sets `ACTIONS_STEP_DEBUG`/`RUNNER_DEBUG`).
  - Delete run from the UI.
  - Job summaries (`$GITHUB_STEP_SUMMARY`) rendered on the run summary page.
  - Bundle and pin a Node 20/24 runtime for JS actions in the runner image, or download and cache it per version.
- **Acceptance**
  - Fixture workflows exercise each checkout input and the cross-run download.
  - The summary renders.
  - The debug rerun shows `##[debug]` lines.
  - `gh run delete` and UI delete work.

### P76 — Polish: Accounts and orgs
- **Priority** 2 · **Wave** W5 · **Migrations** 8800–8899
- **Touches:** `bgh-accounts` (`oauth.rs`, `orgs.rs`), `migrations` (seeds), web (`orgsettings`, `settings`)
- **Scope**
  - Seed the OAuth clients of Git Credential Manager and GitHub Desktop (their public client ids, with redirect and device-flow settings) so browser sign-in works.
  - Org-owned OAuth apps (`/organizations/{org}/settings/applications`) and OAuth app access restrictions (owner approval, request flow).
  - User status: emoji, message, busy, expiry. GraphQL `changeUserStatus` and `User.status`; shown in pickers and hovercards.
  - Org default repository labels (org settings page, applied to new repos).
- **Acceptance**
  - GCM device-flow login against the server succeeds (scripted with GCM's client id).
  - An org with restrictions blocks an unapproved app's token from org resources.
  - Status shows in the assignee picker.
  - A new repo gets the org default labels.

### P77 — Deployment docs and packaging
- **Priority** 2 · **Wave** W6 (last) · **Migrations** 8900–8999 (unused)
- **Touches:** `docs/SELF_HOSTING.md`, `deploy/` (Helm chart, k8s manifests, env example), workspace `Cargo.toml` (redis `tokio-rustls-comp` for `rediss://`), `bgh-core` config (Redis Sentinel optional)
- **Scope**
  - Rewrite SELF_HOSTING.md:
    - SSH (now shipped; the `BGH_SSH_LISTEN` and host key backup).
    - Actions (security model, executors, external runners via `bgh-runner`, OIDC, cache storage).
    - Packages registry hostname and proxy config.
    - Every `BGH_*` env var, generated from code with a test that fails on undocumented vars.
    - Monitoring (P61), backup and restore (P62), private mode.
    - HA guidance: shared storage caveats, sticky WebSocket, per-node state, Redis.
  - Helm chart and plain k8s manifests: Deployment, StatefulSet, PVC, Ingress with SSH TCP, secrets.
  - Redis TLS (`rediss://`).
  - `deploy/bgh.env.example` complete.
- **Acceptance**
  - The docs-vs-code env var test passes.
  - `helm lint` and `helm template` pass.
  - `rediss://` connects in a test with a TLS Redis (or a unit test of the URL handling).
  - A docs review checklist.

---

## 4. Priority 3 packages

### P78 — Polish: Repos and code
- **Priority** 3 · **Wave** W5 · **Migrations** 9000–9099 (unused)
- **Touches:** web `pages/branches`, repo-settings `AccessSettings`, `bgh-repos/src/contents.rs`, `bgh-git` (`languages.rs`, blame)
- **Scope**
  - Branch rename in the branches page.
  - The collaborator picker uses `/search/users`.
  - `web_commit_signoff_required` adds a `Signed-off-by:` trailer to web commits (server side, with the UI note).
  - `.gitattributes` linguist overrides (`linguist-language`, `-vendored`, `-generated`, `-documentation`, `-detectable`) in language stats and in generated-file collapse (P37 hook).
  - Blame honors `.git-blame-ignore-revs` with a toggle.
- **Acceptance**
  - Rename via the UI keeps PRs retargeted.
  - A signoff trailer is present on web commits.
  - Overrides change `/languages`.
  - Blame skips the listed revs.

### P79 — Polish: Issues and wiki
- **Priority** 3 · **Wave** W5 · **Migrations** 9100–9199 (unused)
- **Touches:** `bgh-wiki` (render formats), web `pages/wiki`, `bgh-search` (wiki type)
- **Scope**
  - Render AsciiDoc, Org (`orgize`), reStructuredText, Textile, MediaWiki, Creole and RDoc/POD through available Rust crates, or through optional external renderers with sandboxing and timeouts. Escaped fallback otherwise.
  - Whole-wiki history page `/wiki/_history` and the `_pages` index.
  - Create `_Sidebar` and `_Footer` affordances.
  - Global wiki search type in `/search` (and the API search type if cheap).
- **Acceptance**
  - Fixtures for each format render to HTML.
  - The history page lists commits.
  - Wiki results appear in global search.

### P80 — Gists
- **Priority** 3 · **Wave** W6 · **Migrations** 9200–9299
- **Touches:** new crate `bgh-gists` (git-backed with `bgh-git`), web (`/gist/*` pages), `bgh-accounts/meta` URLs
- **Scope**
  - REST: `/gists` (public, starred, `/users/{u}/gists`), CRUD, `/{id}/commits`, `/{id}/{sha}`, fork, star, comments.
  - Secret gists. Git clone and push over HTTP and SSH.
  - Embeds (`.js`) as a stretch goal.
  - UI: create, list, view, revisions, comments.
- **Acceptance**
  - `gh gist create/list/view/edit/delete` pass.
  - `git clone` of a gist works.
  - Shape tests pass.

### P81 — npm and Maven package registries
- **Priority** 3 · **Wave** W6 (after P15) · **Migrations** 9300–9399
- **Touches:** `bgh-packages`
- **Scope**
  - npm registry: scoped packages `@owner/name`, publish, install, dist-tags, deprecate, unpublish rules. Auth via `_authToken`.
  - Maven registry: `/{owner}/{repo}/…` layout, `maven-metadata.xml`, SNAPSHOT, checksums.
  - Reuse the P15 storage, permissions, REST and UI.
- **Acceptance**
  - `npm publish` and `npm install` against the server (if node is available).
  - A `mvn deploy` fixture via an HTTP-level test.
  - REST lists the packages.

### P82 — Dependency graph, SBOM, dependency review
- **Priority** 3 · **Wave** W6 · **Migrations** 9400–9499
- **Touches:** `bgh-security` (manifest parsers), web Insights dependency graph
- **Scope**
  - Parse manifests and lockfiles on push to the default branch: npm, yarn, pnpm, Cargo, Go, pip, Poetry, Maven, Gradle, Composer, Bundler.
  - `GET /dependency-graph/sbom` (SPDX JSON).
  - `/dependency-graph/compare/{base}...{head}` (for dependency-review-action).
  - Dependency submission API `POST /dependency-graph/snapshots`.
  - Insights → Dependencies page.
- **Acceptance**
  - Fixture repos produce the expected SBOMs.
  - The compare output matches the dependency-review-action expectations.
  - Submission merges with detected dependencies.

### P83 — Dependabot alerts and updates
- **Priority** 3 · **Wave** W6 (after P82) · **Migrations** 9500–9599
- **Touches:** `bgh-security`, `bgh-pulls` (bot PRs)
- **Scope**
  - Advisory DB sync from OSV and the GitHub Advisory Database export, with an offline import option.
  - Alerts: `/dependabot/alerts` for repo and org, PATCH dismiss.
  - `vulnerability-alerts` and `automated-security-fixes` toggles.
  - `/dependabot/secrets`, so `gh secret --app dependabot` works.
  - Security update PRs and `.github/dependabot.yml` version updates for npm, Cargo and Go first, authored by a `dependabot[bot]`-style bot.
  - Webhook `dependabot_alert`.
- **Acceptance**
  - A repo with a vulnerable lockfile gets an alert, and enabling security updates opens a PR that bumps the version.
  - `gh secret set --app dependabot` works.

### P84 — Security advisories and private vulnerability reporting
- **Priority** 3 · **Wave** W6 · **Migrations** 9600–9699
- **Touches:** `bgh-security`, web Security tab
- **Scope**
  - `/repos/{o}/{r}/security-advisories`: draft, collaborators, a temporary private fork for the fix, publish. The CVE field is manual.
  - Private vulnerability reporting by outside users.
  - Webhooks `repository_advisory` and `security_advisory` (local).
- **Acceptance**
  - Draft, private fork, merge fix, publish flow.
  - An outsider's report reaches maintainers only.

### P85 — Code search: symbols, case sensitivity, ranking, code navigation
- **Priority** 3 · **Wave** W6 · **Migrations** 9700–9799
- **Touches:** `bgh-search/src/code`, web `pages/code` and `pages/search`
- **Scope**
  - Symbol index (tree-sitter tags for top languages) built during code indexing.
  - `symbol:` qualifier and `case:yes`.
  - Relevance ranking: matches in path and symbols, test or vendored downranking.
  - Multiple snippets per file.
  - Jump to definition and references in the file view (search-based).
- **Acceptance**
  - `symbol:Foo` finds definitions. `case:yes` excludes other-case hits.
  - Ranking fixture ordering.
  - Click-to-definition works on Rust and TS fixtures.

### P86 — Official actions/runner and ARC compatibility
- **Priority** 3 · **Wave** W6 (after P29) · **Migrations** 9800–9899
- **Touches:** `bgh-actions` (new `_apis/` pipelines and broker protocol module)
- **Scope**
  - Spike, then implement the subset of the `actions/runner` protocol: registration, `/_apis/distributedtask` pools, agents, sessions and messages, job request and renew, timeline and logs upload, plus the broker endpoints of newer runners.
  - `generate-jitconfig` output usable by `actions/runner`.
  - Actions Runner Controller scale-set listener support as a stretch goal.
- **Acceptance**
  - The official `actions/runner` (pinned version) registers with `config.sh --url --token` and runs a simple workflow end to end in CI (when network and binary are available). The protocol is documented in `docs/packages/actions.md`.

---

## 5. Quick fixes, bundled by area

These go into the polish packages instead of becoming their own packages:

- **Pull requests (P74):** dismiss review UI, edit review summary, change base branch, timeline event rendering bugs, deep-link scrolling, merge-dialog defaults, stale delete/restore branch, auto-merge visibility, `allow_update_branch` button visibility (the rebase part is in P40), check output summary.
- **Actions (P75):** checkout and artifact inputs, run timing, debug re-run, delete run UI, job summaries, Node runtime. The badge.svg and stuck check re-run, both higher impact, went into P26.
- **Accounts and orgs (P76):** GCM and Desktop OAuth clients, org-owned OAuth apps and restrictions, user status, org default labels. Org blocked-users UI is in P43. `billing_manager` and leave-org are in P5.
- **Repos and code (P78):** branch rename UI, signoff trailer, collaborator user search, linguist overrides, blame-ignore-revs.
- **Issues and wiki (P79):** wiki formats, wiki history, sidebar and footer, global wiki search.
- **Admin (P63):** maintenance-mode gaps, SSH listen, webhook allowlist UI, test email, global hook deliveries, reindex, admin user edits and reports.
- **API compatibility (P34):** headers, root endpoints, 405 fallback, conditional requests and rate limits, `/meta` SSH keys.
- **Web (P68):** cache persistence, icon chunking, mock exclusion, reload loop, prefetch, virtualization.

## 6. Where merged gaps ended up

Gaps reported in more than one domain:

| Gap (domains) | Package |
|---|---|
| Fork-parent gc corruption, `--prune=now`, no scheduled maintenance, archive cache (repos-git, admin-ops) | P1 |
| Importer and migration tooling (repos-git, admin-ops, web, issues) | P11, P18, P51 |
| `internal` visibility (repos-git, accounts) | P7 |
| Private mode (accounts, admin-ops) | P7 |
| `password_login` not enforced, git basic-auth brute force (accounts, admin-ops) | P14 |
| Signatures and required_signatures (repos-git, pulls, accounts) | P25 |
| Closing keywords and Development section (issues, pulls) | P4 |
| Deployments API (repos-git, pulls, actions, api-compat) | P19, P20 |
| `repository_dispatch` (repos-git, actions, api-compat) | P26 |
| Commit comments (repos-git, api-compat, web) | P32 |
| Rulesets: org, push rules, UI, required workflows and deployments (repos-git, pulls, web, actions) | P3, P23, P24, P20, P30 |
| GraphQL branch protection and Terraform (repos-git, api-compat) | P44 |
| Projects v2 API (issues, api-compat) | P13 |
| Insights and stats endpoints (repos-git, web, api-compat) | P31 |
| Fork, Watch, rename spinner, placeholders, html_url routes (repos-git, web, api-compat) | P12 |
| Invitations acceptance (accounts, web) | P5 |
| Attachments (issues, web) | P6 |
| Markdown parity and autolinks (issues, web) | P35 |
| Dismiss review UI (pulls, web) | P74 |
| Diff highlighting and context expansion (pulls, web) | P37 |
| GitHub Apps (accounts, notify, api-compat) | P17, P46 |
| Fine-grained PATs and narrow scopes (accounts, api-compat) | P47 |
| `workflow` scope and `permissions:` (actions, api-compat) | P8 |
| LDAP, SAML, SCIM and group sync (accounts, admin-ops) | P14, P49 |
| 2FA requirement (accounts, admin-ops) | P36 |
| Event-bus loss (notify, admin-ops) | P9 |
| Global hook deliveries (notify, admin-ops) | P63 |
| Pruning: notifications, deliveries, sessions, activity (notify, admin-ops) | P21 |
| Conditional requests and X-Poll-Interval (notify, api-compat) | P21, P34 |
| CODEOWNERS errors and team review assignment (repos-git, pulls, accounts) | P70 |
| LFS and push quotas and size limits (repos-git, admin-ops) | P2 |
| Discussions (issues, web) | P56, P57 |
| Gists (issues, api-compat) | P80 |
| Security tab and SECURITY.md (actions-security, web) | P66 |
| Check re-run stuck queued (actions) | P26 |
| Check-run pagination and node ids (api-compat) | P22 |
| Environment UI and pending deployments (actions, web) | P20 |
| Settings nav for Actions secrets, runners and environments (web) | P30 |
| Timeline performance (issues, web) | P54 |
| Comment drafts (web) | P54 |
| Saved replies (issues) | P54 |

The full plan is the text above. Nothing was written to disk. The migration ranges come from the contents of `/home/user/better-gh/migrations/`, where files run from 0001 to 1200 and the migrator is defined in `/home/user/better-gh/crates/bgh-core/src/db.rs`.