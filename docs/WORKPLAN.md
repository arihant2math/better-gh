# Work plan

Integration branch: `claude/sleepy-cray-9jj0t3`. Workers develop on their own
branch `bgh/<package>` cut from the integration branch, keep it merged with
the integration branch, and stop when their package's "done" list holds.

Endpoint paths below are relative to `/api/v3` unless noted. "GitHub shape"
means byte-for-byte field compatibility with docs.github.com REST.

## Backend packages

### B1 accounts (`bgh-accounts`, migrations 0100-0199)
Users API (`/user`, `/users/{u}`, `/users`, PATCH `/user`), emails, followers/
following, SSH keys (`/user/keys`), GPG keys, signup/login/logout, password
change + reset (email token), TOTP 2FA + recovery codes, sessions list/revoke,
PATs (classic scopes + expiry), OAuth apps (authorization code flow + device
flow, `/login/oauth/*`), GitHub-compatible `/applications` endpoints, OIDC SSO
login (generic provider config), orgs API (`/orgs/{o}`, `/user/orgs`, members,
memberships, invitations, outside collaborators, org settings, blocking),
teams API (CRUD, nested teams, members, repos, permissions). Avatars
(upload + identicon fallback, cached).

### B2 repos & git (`bgh-repos`, `bgh-git`, migrations 0200-0299)
Repos API (list for user/org/authenticated, PATCH settings, transfer,
topics, languages, contributors, tags, teams, forks + `POST forks`,
templates/generate, stargazers + `/user/starred`, subscribers +
subscription), collaborators + invitations, contents API (GET/PUT/DELETE
file, readme, `tarball`/`zipball`), git database API (blobs, trees,
commits, refs, tags), commits (list w/ sha/path/author/since filters, get
with files+stats, compare `base...head`), branches (list/get/rename,
protection CRUD incl required reviews/status checks/admin enforcement/
restrictions, rulesets-lite), deploy keys, SSH git transport (russh, keys from
users/deploy keys), Git LFS (batch API, locks optional), raw file route,
web endpoints for code browsing: `/_bgh/repos/{o}/{r}/tree/{ref}/{path}`,
blob with server-side syntax highlight (cached by blob sha), blame, file
history, README rendering. Branch protection enforcement in receive-pack.

### B3 issues (`bgh-issues`, migrations 0300-0399)
Issues CRUD + list (repo/user/org/`/issues` filters, sort, since),
labels (repo + issue), milestones, assignees, comments CRUD, reactions (issues,
comments), lock/unlock, events + timeline APIs, issue templates (from
`.github/ISSUE_TEMPLATE`), sub-issues, pinned issues, transfer, mention/
cross-reference events. All writes recorded to sync_actions.

### B4 pulls (`bgh-pulls`, migrations 0400-0499)
PR CRUD + list, `.diff`/`.patch` media types, files, commits, merge
(merge/squash/rebase with `git merge-tree --write-tree`, conflict detection),
mergeability background job on push, update branch, reviews (pending/submit/
dismiss), review comments (line/multi-line, replies, resolve threads),
requested reviewers (users/teams), CODEOWNERS auto-request, branch
protection checks on merge, commit statuses + combined status, checks API
(suites/runs/annotations), auto-merge, draft/ready, PR timeline events.

### B5 notify (`bgh-notify`, migrations 0500-0599)
Notifications API (threads, mark read, subscriptions), participation
reasons (author, mention, review_requested, assign, comment, subscribed,
team_mention), repo watching semantics, webhooks (repo + org, CRUD, ping,
GitHub payload shapes + `X-GitHub-Event`/`X-Hub-Signature-256`, delivery log,
redelivery, retries via jobs), email notifications (SMTP via lettre, digest
optional), mention parsing.

### B6 releases, search, activity (`bgh-releases`, `bgh-search`)
Releases CRUD + assets upload/download (`uploads` path), generate notes,
latest/by tag; search API (`/search/issues`, `/search/repositories`,
`/search/users`, `/search/code`, `/search/commits`) with GitHub query
syntax (`is:`, `label:`, `author:` ...) — Postgres FTS + trigram, code search
via an indexed trigram store or git grep per repo; events/activity API
(`/events`, `/users/{u}/events`, `/repos/{o}/{r}/events`, `/orgs/{o}/events`,
received_events) for the dashboard feed.

### B7 admin (`bgh-admin`, migrations 0800-0899)
GHES-style admin API (`/admin/users`, `/admin/organizations`, suspend/
unsuspend, promote/demote site admin, impersonation tokens, `/enterprise/
stats/*`), site settings (signup policy, default visibility, auth providers,
SMTP, announcement banner, rate limits), audit log API + search, background
job inspector (list/retry/cancel), system health (db/redis/disk), repo
maintenance (gc, fsck, reindex), user/org/repo management (rename, delete,
transfer, quotas).

### B8 sync (`bgh-sync`, migrations 0900-0999)
Implements `docs/SYNC_PROTOCOL.md` exactly: bootstrap, partial sync, WS
deltas with Redis pubsub fan-out, permission checks per scope, resumption,
compaction of old sync_actions.

### B9 graphql (`bgh-graphql`)
async-graphql schema covering what `gh` CLI uses: viewer, repository (issues,
pullRequests, labels, milestones, refs, defaultBranchRef, collaborators),
issue/PR queries + mutations (create, close, reopen, merge, comment, review,
add labels/assignees), search, node(id), pagination connections.

### B10 actions (`bgh-actions`, migrations 1000-1099)
GitHub Actions compatible subset: parse `.github/workflows/*.yml` (on: push/
pull_request/workflow_dispatch/schedule; jobs, needs, matrix, env, secrets,
`runs-on`, `steps` w/ `run` and common `uses`), runner (docker-based, built-in
local runner + registration API for remote runners), logs streaming, artifacts,
secrets/variables (repo/org, encrypted), check runs/statuses integration,
REST `/actions/*` endpoints (runs, jobs, logs, re-run, cancel, secrets).

### B11 projects & wiki (`bgh-projects`? lives in bgh-issues or new crate)
Projects (v2-style boards: items = issues/PRs/drafts, custom fields
status/iteration/number/text, views table/board), wiki (git-backed
`{repo}.wiki.git`, pages CRUD, history), discussions (optional), gists
(optional).

## Frontend packages (after backend APIs land)

F1 auth/settings/profile/org/team pages; F2 code browser (tree, blob,
blame, history, commits, compare, branches, tags, releases); F3 issues
(list/detail/labels/milestones/projects board); F4 PRs (conversation,
files changed w/ inline review, commits, checks, merge box); F5
notifications/dashboard/search; F6 repo + org settings (collaborators,
branches protection, webhooks, deploy keys, secrets, actions); F7 admin UI;
F8 actions UI (runs, logs streaming).
