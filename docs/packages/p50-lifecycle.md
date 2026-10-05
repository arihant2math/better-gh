Integration: landed
P50 account and repo lifecycle: self-service rename with redirects, account/org deletion, repo soft delete + restore, transfers to users with acceptance.

# P50 lifecycle — status

Branch `bgh/p50-lifecycle`. Scope: `docs/PHASE4_PLAN.md` §P50 (no §5 quick
fixes are assigned to P50). Migration `6200_lifecycle.sql`.

## Backend

### Shared (`bgh_core::lifecycle`, new module)
* `resolve_owner(login)` (direct login, else `login_redirects`) and
  `resolve_repo(owner, name)` (direct, else `repo_redirects`, else same name
  under a renamed owner). Used by `RepoAccess::load` (REST, git smart HTTP),
  SSH deploy-key auth, LFS `RemoteAuth`, the OCI registry `/v2/{owner}/{pkg}`
  and Packages REST owner lookup (P15), `bgh_accounts::util::find_account`
  (`/users/{u}`, `/orgs/{o}`, ...). This extends P12's `repo_redirects`
  mechanism; there is no second redirect table for repos.
* `rename_account_in(tx, account, login)`: updates the login, inserts a
  `repo_redirects` row `old_login/name → id` for every owned repo (dropping
  redirects that would shadow the new names), a `login_redirects` row
  reserved 90 days, syncs repos + user/org. The site-admin rename
  (`bgh_admin::service::rename_account`) now uses it, so admin renames get
  redirects too.
* Login reservation is a `BEFORE INSERT OR UPDATE OF login` trigger on
  `users`: taking a reserved login raises `unique_violation` on
  `users_login_key`, so every creation path (sign-up, admin, SSO/LDAP,
  importer) returns its existing 422 `already_exists`. Reclaiming your own
  old login (or an expired one) drops the redirect.
* `soft_delete_repo_in` / `restore_repo_in` with `lifecycle::snapshot`:
  generic capture of every row cascading from the `repositories` row (FK
  graph read from `pg_catalog`), plus `SET NULL` references to them
  (forks' `parent_id`, ...). Restore re-inserts in FK order with
  `OVERRIDING SYSTEM VALUE`, nulls `SET NULL` references whose targets are
  gone (ghost), skips rows whose cascade targets are gone (row-by-row
  fallback with savepoints), then re-links. Excluded (rebuilt or
  ephemeral): code index tables, maintenance bookkeeping, `repo_transfers`,
  `webhook_deliveries`. Restore recounts forks/stars, syncs the repo and
  enqueues `search.index_repo`.
* `delete_account_in` (soft-deletes owned repos, deletes the row; authored
  content renders as `ghost` via the existing `SET NULL` FKs) and
  `sole_owned_orgs`. Admin `delete_repo_in`/`delete_account` now soft-delete.
* `mail::templates::repo_transfer`; `testing::TestApp::purge_deleted_repos`.

### Endpoints
| Endpoint | Notes |
|---|---|
| `PATCH /user {login}` | self rename (scope `user`); 422 `invalid`/`already_exists`; 429 after 3 renames / 24 h; audit `user.rename`, `UserAccountChanged{renamed}` |
| `PATCH /orgs/{org} {login}` | org owners; same rules; `org.rename` |
| `DELETE /user {password}` | browser session + password (or 2FA code for password-less accounts); 403 "Incorrect password."; 422 when sole owner of an org or last site admin; 204, sessions ended |
| `DELETE /orgs/{org}` | owners → 202 `{}`; org repos soft-deleted |
| `GET /users/{old}` | 301 → `/user/{id}` |
| `GET /repos/{old}/{name}` | 301 → `/repositories/{id}` (GitHub's shape: `Location`, `{message, url, documentation_url}`); sub-resources and git resolve transparently |
| `DELETE /repos/{o}/{r}` | soft delete (unchanged 204); name freed at once |
| `GET /_bgh/repos/deleted[?owner=]` | caller's and owned orgs' deleted repos: `{id, name, full_name, owner{id,login,type}, visibility, private, fork, deleted_at, purge_at, deleted_by, restorable}` |
| `GET /_bgh/admin/repos/deleted` | site admin, all |
| `POST /_bgh/repos/{id}/restore` | owner / org owner / site admin → 200 Repository; 422 name taken or owner gone; 404 otherwise |
| `POST /repos/{o}/{r}/transfer` to another user | 202 with the unchanged repo; creates a pending `repo_transfers` row (1 day), emails the recipient a link to `/settings/repositories/transfers?id=N`. Orgs / yourself: immediate as before |
| `GET`/`DELETE /_bgh/repos/{o}/{r}/transfer` | pending transfer (repo admins), cancel |
| `GET /_bgh/user/repo_transfers`, `POST …/{id}/accept`, `POST …/{id}/decline` | recipient; accept → 200 Repository (moved, old name redirects), expired → 410 |

Service `repos.purge_deleted` (hourly, `bgh_core::lifecycle::purge_expired`):
purges repos past `purge_after`
(enqueues `repos.delete_storage`, `wiki.delete_storage`, LFS + uploads GC)
and expired transfers. `repos.delete_storage`, `wiki.delete_storage`, the
LFS GC and the uploads GC never touch data of a repo still in
`deleted_repositories` (`lfs_oids`, `blob_shas` columns).

### Tables (6200)
`login_redirects`, trigger `users_login_reservation`,
`deleted_repositories`, `repo_transfers`.

## Web
* Account settings: Change username dialog, Delete account dialog
  (type login + password).
* Org settings: Danger zone (rename, delete; owners only).
* `/settings/repositories/deleted` (restore, owner filter for owned orgs),
  `/settings/repositories/transfers` (accept/decline, `?id=` highlight),
  site admin `/site-admin/repos/deleted`.
* Repo settings transfer: pending banner with Cancel for user transfers.
* Profile `/:owner` replaces old logins with the canonical one
  (`pages/profile/canonical.ts`); P12 already handled repo URLs.
* All lazy chunks; initial bundle 144.4 → 144.6 KB gzip (route entries).
  Mocks in `web/src/mock/extra/lifecycle.ts`. Verified with Playwright in
  mock mode.

## Tests
`bgh-repos` `lifecycle::*` (rename → old git remote clone/push + 301 +
reservation, delete/restore round trip with issues/labels/comments/stars/git,
purge, user transfer accept/expire/decline/cancel, admin restore);
`bgh-accounts` `lifecycle::*` (rename validation + rate limit, org
rename/delete, account deletion → ghost, sole-owner/password checks);
`bgh-packages` `renames::*` (old owner image path pulls).
Test helper `TestApp::purge_deleted_repos()` (ends retention, purges, drains
jobs). Adapted to soft delete / 301: `bgh-admin`
`ghes_users::renames_and_deletes_users`; `bgh-repos` `api::delete_repository`,
`forks::deleting_the_source_keeps_forks_working`,
`lfs::raw_resolves_pointers_and_gc`,
`maintenance::deleting_parent_makes_forks_self_contained`,
`settings::rename_keeps_redirect`, `settings::transfer_to_org`; `bgh-uploads`
`uploads::repo_deletion_removes_attachments_and_blobs`; `bgh-wiki`
`pages::repository_deletion_removes_wiki_storage` (storage/blobs now survive
until the purge, which they assert).

## Gate
Full gate green after merging `claude/sleepy-cray-9jj0t3` (fmt, clippy,
`cargo test --workspace`, web typecheck/lint/test/build: initial JS 144.9 KB
gzip). `scripts/api-smoke.sh` 45/45. `scripts/gh-compat.sh` 69 passed, 2
failed: `gh ruleset list/view --org`, a pre-existing script issue (since the
P23 merge it creates `$OWNER-org` twice; the second create 422s and resets
`FX_ORG` to empty, so those cases query org `x`). Not P50 code.

## Known gaps
* `DELETE /user` uses password re-confirmation; P36 sudo mode was not
  available — switch to it when it lands.
* Restoring repos of a deleted account is not possible (owner gone → 422).
* Restored rows referencing accounts deleted meanwhile lose those rows
  (e.g. an assignee) or render ghost (authors).
* No `repository` webhook action for restore (GitHub has none).
