# B7 admin — status

Crate `bgh-admin`, migrations `0800-0899` (`0800_admin.sql`), branch
`bgh/admin`. **Status: complete** (optional pre-receive hooks and LDAP not
implemented, see gaps).

Every endpoint requires a site administrator (`RequireSiteAdmin`: tokens
need the `site_admin` scope; sessions have every scope) unless noted, and
every change writes an audit entry with the caller's IP.

## GHES-compatible REST API (`/api/v3`)

| Endpoint | Notes |
|---|---|
| `POST /admin/users` | `{login, email?, suspended?}` → 201 simple-user; passwordless account |
| `PATCH /admin/users/{u}` | `{login}` → 202 GHES "Job queued" shape (applied immediately) |
| `DELETE /admin/users/{u}` | 204; owned repos deleted (storage via `repos.delete_storage`), authored content → ghost; can't delete yourself |
| `POST /admin/users/{u}/authorizations` | impersonation token (`access_tokens.kind = 'impersonation'`, `created_by_id`) → 201 `authorization`; 200 with empty `token` if same scopes exist; `site_admin` scope refused |
| `DELETE /admin/users/{u}/authorizations` | 204, revokes all impersonation tokens |
| `PUT/DELETE /users/{u}/site_admin` | 204; last site admin can't be demoted (422) |
| `PUT/DELETE /users/{u}/suspended` | `{reason}` → 204; suspending destroys sessions; admins/self can't be suspended (403) |
| `PATCH /admin/organizations/{org}` | `{login}` → 202 |
| `GET /admin/keys`, `DELETE /admin/keys/{id}` | user SSH keys + deploy keys (`public-key-full`), `since`/`sort`/`direction`; user key wins on id clash |
| `GET/POST /admin/hooks`, `GET/PATCH/DELETE /admin/hooks/{id}`, `POST /admin/hooks/{id}/pings` | global webhooks (`webhooks` rows with no repo/org), `global-hook` shape, secret write-only |
| `GET /enterprise/stats/{all,repos,hooks,pages,orgs,users,pulls,issues,milestones,gists,comments}` | GHES shapes; `total_pushes` from `site_counters` (Push event listener); gists = 0 |
| `GET /enterprise/settings/license` | unlimited seats, `seats_used` = active users, no expiry |
| `GET/PATCH/DELETE /enterprise/announcement` | `{announcement, expires_at, user_dismissible}` |
| `GET /rate_limit` | any caller; 404 "Rate limiting is not enabled." when disabled |
| `GET /orgs/{org}/audit-log` | org owners (or site admins); tokens need `read:audit_log` or `admin:org`; GitHub entry shape (`@timestamp`, `_document_id`, `action`, `actor`, `org`, `repo`, `user`, flattened data); `phrase`, `include`, `after`/`before` cursors + `Link`, `order`, `per_page` |
| `GET /enterprises/{e}/audit-log` | whole instance, includes `actor_ip` |

## Admin UI endpoints (`/_bgh/...`)

* `GET /_bgh/site` (public): site name, active announcement, maintenance
  state, sign-up policy, sign-in methods (no secrets).
* `GET/PATCH /_bgh/admin/settings`: all sections; PATCH merges fields per
  section, validates the whole, secrets are returned as `********` and
  sending the placeholder back keeps the stored value.
* `GET /_bgh/admin/audit-log`: filters `actor`, `action` (exact or
  category), `user`, `org`, `repo` (`owner/name`), `since`/`until`, `phrase`
  (GitHub syntax incl. `created:>=…`, ranges), `order`, cursor pagination
  (`cursor`, `next_cursor`, `Link: next`).
* Jobs: `GET /_bgh/admin/jobs[?state=pending|scheduled|running|failed&kind=]`,
  `GET /_bgh/admin/jobs/stats`, `GET /_bgh/admin/jobs/{id}`,
  `POST /_bgh/admin/jobs/{id}/retry` (fresh attempts, 409 if running),
  `POST /_bgh/admin/jobs/{id}/cancel` (deletes, 409 if running),
  `POST /_bgh/admin/jobs/retry-failed[?kind=]`.
* `GET /_bgh/admin/health`: database (version, latency, size, pool), Redis,
  storage (`df` of data_dir, repo bytes), git version, queue depth/oldest
  ready job, uptime, version; overall `ok|degraded|error`.
* Maintenance: `POST/GET /_bgh/admin/repos/{o}/{r}/maintenance`
  (`gc|repack|fsck|recalculate_size|recalculate_languages`, job
  `admin.repo_maintenance`, results in `repo_maintenance_runs`),
  `POST /_bgh/admin/maintenance` (all repos, set-based enqueue).
* Users: `GET /_bgh/admin/users` (`q`, `type`, `filter=admin|suspended|active|dormant|2fa|no_2fa`,
  `sort=login|created|last_active|repos|disk_usage`, totals + `last` link),
  `POST /_bgh/admin/users` (with or without password),
  `GET/PATCH/DELETE /_bgh/admin/users/{u}` (details: emails, SSH/GPG keys,
  repos, orgs, 2FA, sessions, tokens, quota; PATCH login/site_admin/suspended;
  DELETE `?transfer_repositories_to=`), `POST …/password` (set or generate,
  signs out), `DELETE …/two-factor`, `DELETE …/sessions`.
* Orgs: `GET/POST /_bgh/admin/orgs`, `GET/PATCH/DELETE /_bgh/admin/orgs/{o}`
  (PATCH `login`, `archived`; DELETE with transfer).
* Repos: `GET /_bgh/admin/repos` (`q`, `owner`, `visibility`, `archived`,
  `disabled`, `fork`, sort incl. `size`), `GET/PATCH/DELETE
  /_bgh/admin/repos/{o}/{r}` (PATCH `name`, `visibility`, `archived`,
  `disabled`), `POST …/transfer` (`new_owner`, `new_name`; drops team
  grants, internal → private for users).
* Quotas: `GET/PUT/DELETE /_bgh/admin/accounts/{login}/quota`
  (`max_repo_size_mb`, `max_total_size_mb`; usage + effective limits).

## Tables / migrations (`0800_admin.sql`)

`access_tokens.kind` += `impersonation`, `access_tokens.created_by_id`;
`user_two_factor` (2FA state, see below); `storage_quotas`;
`repo_maintenance_runs`; `site_counters`; audit-log id-cursor indexes
(org/repo/actor/action-prefix/target); job inspector indexes; user/repo
admin listing indexes.

## Shared-code changes (all additive)

* `bgh_core::settings` (new): typed `SiteSettings` (signup policy
  open/invite/closed + allowed email domains, default repo visibility, max
  repo size, org creation policy, announcement with expiry, rate limits,
  auth providers incl. OIDC list, SMTP, maintenance), per-process 5 s cache
  (`load`, `invalidate`), `check_signup`, `can_create_org`,
  `default_visibility`, `storage_limits_kb`, `check_push_quota`,
  `maintenance_middleware`, `public_info`.
* `bgh_core::ratelimit` (new): Redis hourly-window middleware on `/api/v3`
  (`X-RateLimit-*`, 403 when exceeded), `quota()` for `/rate_limit`.
* `bgh_core::two_factor` (new): `enabled_at`, `disable` over `user_two_factor`.
* `bgh_core::auth`: `resolve_request` (middleware auth resolution, cached
  for extractors), `client_ip`.
* `bgh_core::audit`: `log_with_ip` (`log` delegates to it).
* `bgh_core::events`: `UserAccountChanged`, `OrganizationChanged`,
  `GlobalHookPing` (global webhook sources for B5).
* `bgh_core::perms::RepoAccess::for_repo`: repositories with `disabled`
  are 403 "Repository access blocked" for non-site-admins.
* `bgh-server`: maintenance middleware (root) and rate-limit middleware
  (`/api/v3`).
* `bgh-accounts`: sign-up calls `settings::check_signup`; login success /
  failure and logout are audit-logged with IP; `create_user` /
  `create_org` emit `UserAccountChanged` / `OrganizationChanged`.
* `bgh-repos`: repo creation uses the default-visibility setting when the
  request specifies none; receive-pack calls `settings::check_push_quota`.

## Notes for other packages

* **B1 accounts**: store TOTP state in `user_two_factor` (columns `method`,
  `secret`, `recovery_codes`, `enabled_at`); OIDC providers and
  password-login toggle come from `settings::load(..).auth_providers`;
  user-facing org creation must check `SiteSettings::can_create_org`.
  Audit new writes (emails, keys, memberships, teams) with `audit::log`.
* **B2 repos**: SSH transport must reject suspended users (HTTP already
  does via `authenticate`) and call `check_push_quota`; PATCH
  visibility/transfer should log `repo.access` / `repo.transfer`, and
  protection changes `protected_branch.*`.
* **B5 notify**: deliver global hooks (`webhooks` with `repo_id IS NULL AND
  org_id IS NULL`) for `UserAccountChanged` (`user` event),
  `OrganizationChanged` (`organization`) and `GlobalHookPing` (`ping`);
  SMTP config is `settings::load(..).smtp`.

## Known gaps / TODO

* Pre-receive hook environments (`/admin/pre-receive-*`) and LDAP sync
  (`/admin/ldap/*`) are not implemented (optional in the plan).
* Audit `include=git` events are not recorded (only web events).
* Gist stats are zero until gists exist; `total_wikis` counts repos with
  `has_wiki`.
* Maintenance language detection is an extension-based subset of linguist.
