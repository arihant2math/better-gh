# Package B2a: repos-api

Branch `bgh/repos-api` · crates `bgh-repos` (+ additive `bgh-git`) ·
migrations 0200-0249 (used: `0200_repos_api.sql`).

## Status

Scope implemented and tested (see endpoint list). Every endpoint has
integration tests asserting GitHub JSON shapes and status codes
(`crates/bgh-repos/tests/*.rs`).

## Endpoints (relative to `/api/v3`)

Repositories (`repos.rs`, `settings.rs`, `create.rs`)
* `GET /user/repos` (`visibility`, `affiliation`, `type`, `sort`, `direction`, `since`, `before`), `GET /users/{u}/repos` (`type=owner|member|all`), `GET /orgs/{org}/repos` (`type=all|public|private|forks|sources|member`)
* `PATCH /repos/{o}/{r}`: name (rename + redirect), description/homepage (null clears), private/visibility, has_*, is_template, default_branch (moves HEAD), merge options, squash/merge commit title/message, archived (archived repos only accept unarchive), allow_forking, web_commit_signoff_required
* `POST /repos/{o}/{r}/transfer` (202; immediate when the caller can create repos in the target; `new_name`, `team_ids`), old name redirects
* `GET`/`PUT /repos/{o}/{r}/topics`

Derived data (`stats.rs`)
* `GET /repos/{o}/{r}/languages`: `repos.compute_languages` job (enqueued by post-receive when the default branch moves, and on stale reads), linguist-like map in `bgh_git::languages` (programming/markup only; vendored/docs/generated excluded); updates `repositories.language`
* `GET /repos/{o}/{r}/contributors` (`anon`), shortlog cached by head SHA, emails → users batch mapped
* `GET /repos/{o}/{r}/tags` (natural version order), `GET /repos/{o}/{r}/teams`

Forks & templates (`forks.rs`)
* `GET`/`POST /repos/{o}/{r}/forks` (`organization`, `name`, `default_branch_only`; returns the existing fork in the network)
* `POST /repos/{o}/{r}/generate` (`owner`, `name`, `description`, `include_all_branches`, `private`): fresh "Initial commit" per branch, objects repacked from the template; `template_repository` in the full repo JSON
* Fork deletion fix: `DeleteStorage { forks }` dissociates direct forks (`repack -a -d` + drop alternates) before removing storage

Stars / watching (`stars.rs`, `watching.rs`)
* `GET /repos/{o}/{r}/stargazers`, `GET /user/starred`, `GET /users/{u}/starred` (star+json media type), `GET|PUT|DELETE /user/starred/{o}/{r}`
* `GET /repos/{o}/{r}/subscribers`, `GET /user/subscriptions`, `GET /users/{u}/subscriptions`, legacy `/user/subscriptions/{o}/{r}` (maintains `watchers_count`). `/repos/{o}/{r}/subscription` was implemented then **removed** at the orchestrator's request: it belongs to bgh-notify (B5), which must keep `repositories.watchers_count` in sync when it writes `watches`

Collaborators (`collaborators.rs`)
* `GET /repos/{o}/{r}/collaborators` (`affiliation`, `permission`), `GET|PUT|DELETE .../collaborators/{u}`, `GET .../collaborators/{u}/permission`
* `GET /repos/{o}/{r}/invitations`, `PATCH|DELETE .../invitations/{id}`, `GET /user/repository_invitations`, `PATCH|DELETE /user/repository_invitations/{id}`

Contents (`contents.rs`)
* `GET /repos/{o}/{r}/contents[/{path}]` (file/dir/symlink/submodule, `?ref`, raw/html/object media types), `PUT` (create/update with sha check, author/committer), `DELETE`
* `GET /repos/{o}/{r}/readme[/{dir}]`
* `download_url` points at `/{owner}/{repo}/raw/{ref}/{path}`, served by B2b's download module (not registered here, to avoid a duplicate route)

Git database (`gitdb.rs`)
* blobs (GET/POST), trees (GET `recursive`, POST with `base_tree`/deletes), commits (GET/POST), refs (`/git/ref/{ref}`, `/git/matching-refs/{ref}`, `/git/refs[/{prefix}]`, POST/PATCH(force)/DELETE), annotated tags (GET/POST)

Commits (`commits.rs`)
* `GET /repos/{o}/{r}/commits` (`sha`, `path`, `author`/`committer` login or email, `since`, `until`, pagination)
* `GET /repos/{o}/{r}/commits/{ref}` (files + stats + patches; `.diff`, `.patch`, `.sha` media types; immutable cache headers for full SHAs)
* `GET /repos/{o}/{r}/compare/{base}...{head}` (`owner:ref`, `owner:repo:ref` cross-fork; commits oldest first ≤250, paginated on request; `.diff`/`.patch`)

Branches & merges (`branches.rs`)
* `GET /repos/{o}/{r}/branches` (`protected`), `GET /repos/{o}/{r}/branches/{branch}` (slashes allowed), `POST .../branches/{branch}/rename` (protection rules, default branch and open PR refs follow)
* `POST /repos/{o}/{r}/merges` (201/204/404/409), `POST /repos/{o}/{r}/merge-upstream` (fast-forward / merge / none, 409 on conflicts)

Branch protection & rulesets (`protection_api.rs`, `rulesets.rs`, engine `protection.rs`)
* `GET|PUT|DELETE .../branches/{b}/protection` and all sub-resources (required_status_checks (+contexts), enforce_admins, required_pull_request_reviews, required_signatures, restrictions (+users/teams/apps))
* `GET|POST /repos/{o}/{r}/rulesets`, `GET|PUT|DELETE .../rulesets/{id}`, `GET /repos/{o}/{r}/rules/branches/{branch}`
* Enforcement on push (`git_http.rs`): rules evaluated on the command list; required status checks from `commit_statuses`/`check_runs`; force-push and linear-history checks run in a quarantined `pre-receive` hook. API ref writes use the same engine (`refs::write_ref`).

Deploy keys & autolinks (`keys.rs`, `autolinks.rs`)
* `GET|POST /repos/{o}/{r}/keys`, `GET|DELETE .../keys/{id}` (key validation, SHA256 fingerprint, unique across user keys)
* `GET|POST /repos/{o}/{r}/autolinks`, `GET|DELETE .../autolinks/{id}`

## Tables / migrations

`0200_repos_api.sql`: `repo_redirects`, `repo_languages`, `repo_rulesets`,
`repo_autolinks`; indexes for stars/watches/forks/invitations/deploy-key lists.

## Shared-code changes (additive)

* `bgh_core::perms::RepoAccess::load` falls back to `repo_redirects` (renamed/transferred repos keep resolving for every crate and git transport).
* `bgh_core::events::Event`: `RepositoryStarred`, `RepositoryForked`, `RepositoryRenamed`, `RepositoryTransferred`, `CollaboratorAdded`.
* `bgh_git::ops` (new): `GitCli` (`RepoStore::cli`), `LogFilter`, `DiffFile`, `LsTreeEntry`, `TreeEdit`/`build_tree` (mktree), `MergeOutcome`/`merge_trees`, `commit_tree`, `write_tag`, `cat_objects`, `fetch_objects`, `dissociate`, `with_objects_of`.
* `bgh_git::languages` (new), `bgh_git::smart_http::{PushPolicy, receive_pack_with_policy, ensure_hooks, PRE_RECEIVE_HOOK}` (`receive_pack` unchanged, delegates).
* `bgh_git::objects`: `Deserialize` derives (for caching).
* `bgh-server` `api_headers`: `X-GitHub-Media-Type` is only defaulted (no longer overwrites `param=raw|html|diff|...` set by handlers).
* Public helpers other crates may use: `bgh_repos::protection::{RepoRules, Actor, check_update, missing_status_checks}` (B4: merge checks), `bgh_repos::refs::write_ref`, `bgh_repos::keys::parse_public_key`.

## Caching

Redis (`bgh_repos::cache`, prefix `gitcache:`, 7-day TTL, keys are SHAs):
commit lists (`log:{head}:{filters}`), diffs (`diff:{from}:{to}`), compares,
shortlogs, recursive trees, language tallies (by tree), contents JSON.
SHA-addressed responses (commits/blobs/trees by full SHA) send
`Cache-Control: public|private, max-age=31536000, immutable`.

## Known gaps / deviations

* Transfers are immediate (no acceptance flow for transfers to other users; those return 403).
* Commit signatures are reported (`verification.signature`) but not verified (`reason: unknown_key`).
* Branch rename updates `pull_requests.base_ref/head_ref` of open PRs directly (B4 may want to emit timeline events).
* List endpoints filtered by readability after paging (`/user/starred`, `/users/{u}/starred`, subscriptions) can return short pages.
* Ruleset `bypass_mode: pull_request`, `required_signatures` and `update_allows_fetch_and_merge` are stored but not enforced; classic `required_signatures` likewise.
* Contents: submodules in directory arrays are `type: "file"` (GitHub's documented compat behavior); symlinks to files resolve to the target file; `download_url` carries no token for private repos.
* Short SHAs are not accepted by `git/blobs|commits|tags/{sha}`.
* Restriction / bypass users aren't checked for push access on PUT; `apps` are always empty.
* Coordination with `bgh/git-transport` (B2b), for the merge:
  * B2b owns `/{owner}/{repo}/raw/...`; repos-api registers no web routes of its own besides git HTTP.
  * `protection.rs` was rewritten as a rules engine (classic rules + rulesets). B2b's edits to the old `check_push` will conflict textually: keep this branch's file. Compat shims `load_rules(state, repo_id)`, `check_push`, `check_push_by(rules, access, Option<user_id>, updates)` keep B2b's SSH code compiling; SSH pushes should move to `Actor::load` + `authorize_push` + `smart_http::receive_pack_with_policy` to get force-push/linear-history/status-check enforcement (deploy keys: `Actor { user_id: 0, .. }`).
  * `git_http.rs` receive-pack was changed here to the policy-based path; on conflict keep this branch's version plus B2b's other additions.
  * This branch's git test helpers live in `tests/gitwork/` (B2b uses `tests/common/`).
