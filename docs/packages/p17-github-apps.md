# P17 — GitHub Apps, part 1: registration, JWT, installations, installation tokens — status

**In progress.** Branch `bgh/p17-github-apps`. Migration
`2900_github_apps.sql` (range 2900–2999).

## What exists

* **Registration** (`bgh_accounts::apps::manage`, web JSON, browser session
  required for writes so a token can't mint itself an app with more access
  than its scopes): apps owned by a user or an organization (org admins),
  name/slug (unique, slug = lowercase alphanumerics joined by `-`, ≤ 34
  chars), description, homepage, up to 10 callback URLs, setup URL and
  "redirect on update", webhook active/URL/secret (sealed with
  `bgh_core::secretbox`, write-only: `webhook_secret_set`), permissions
  (GitHub's repository/organization/account names, `read|write|admin`
  validated per permission, `metadata: read` implied), events (validated
  list), public/private, client id `Iv23…`. Renaming changes the slug and
  the bot login.
* **Bot user** `{slug}[bot]` (`users.type = 'Bot'`) per app. Deleting the
  app deletes the bot (cascades app, installations, tokens; its content
  renders as ghost).
* **Private keys**: 2048-bit RSA generated server-side (`apps::generate_key`
  on the blocking pool); the PKCS#1 PEM is returned once (UI downloads
  it), only the SPKI public key and `SHA256:` fingerprint are stored.
  Multiple keys; deleting one invalidates JWTs signed with it.
* **App JWT** (`bgh_core::apps::jwt_auth`, from `auth::token_auth` when the
  bearer looks like a JWT): RS256 only, `iss` = app id (number or string)
  or client id, `exp` in the future and ≤ now + 600 s (60 s leeway), `iat`
  not in the future; GitHub's 401 messages ("'Expiration time' claim
  ('exp') is too far in the future", "Integration not found", "A JSON web
  token could not be decoded", …). The caller is `AuthMethod::App { app_id }`
  acting as the bot, with no scopes; `apps::middleware` (mounted in
  bgh-server) answers 403 "Resource not accessible by integration" outside
  the app endpoints (`apps::jwt_route`). Non-JWT credentials on JWT
  endpoints get 401 "A JSON web token could not be decoded".
* **Installations**: `app_installations` (all/selected, accepted
  permissions and events snapshot, installer, suspension) and
  `app_installation_repos`. Private apps install only on their owner.
* **Installation tokens**: `bghs_` + 40 alphanumerics, 1 h, `access_tokens`
  rows of kind `app` with `installation_id`, owned by the bot. Scopes:
  `app:installation:{id}`, `app:owner:{account}` (all repositories) or
  `app:repo:{id}` per repository, and `actions:permission:{cat}:{access}`
  (P8's `token_permissions` encoding). `perms::effective` →
  `apps::effective_cap`: covered repositories get Write (any write
  permission) or Read; others are seen like an anonymous caller (private →
  404). Token writes are authored by the bot. Removing a repository from an
  installation strips it from live tokens; switching to selected,
  suspending or uninstalling revokes them. `X-OAuth-Scopes` is not sent for
  app credentials.
* **Git**: `x-access-token:<token>` (any username) over HTTPS and LFS;
  `apps::check_git` needs `contents: write` to push, `contents: read` to
  fetch private repositories.
* **Rate limits**: installation tokens count against `i:{installation}`,
  app JWTs against `a:{app}` (`ratelimit::caller_key`).

## Endpoints

GitHub REST (shapes `integration` / `installation` in `bgh_core::apps`,
re-exported from `models::api`):

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /app` | JWT | with `installations_count` |
| `GET /apps/{slug}` | any | private apps: their own credentials and owner admins, else 404 |
| `GET /app/installations` | JWT | `since`, `outdated` accepted; paginated (`Link`) |
| `GET`/`DELETE /app/installations/{id}` | JWT | 404 for other apps' installations |
| `PUT`/`DELETE /app/installations/{id}/suspended` | JWT | 204; suspension revokes tokens |
| `POST /app/installations/{id}/access_tokens` | JWT | `repositories` (names), `repository_ids`, `permissions`; 422 "There is at least one repository that does not exist or is not accessible to the parent installation." / "The permissions requested are not granted to this installation."; 403 "This installation has been suspended"; 201 `{token, expires_at, permissions, repository_selection, repositories?}` |
| `GET /orgs/{org}/installation`, `/users/{u}/installation`, `/repos/{o}/{r}/installation` | JWT | 404 when not installed / repository not covered |
| `GET /installation/repositories` | installation token | `{total_count, repository_selection, repositories}` |
| `DELETE /installation/token` | installation token | 204 |
| `GET /user/installations` | user | installations on the viewer's account and its orgs; `{total_count, installations}` + `Link` |
| `GET /user/installations/{id}/repositories` | user | installation repositories the viewer can read |
| `PUT`/`DELETE /user/installations/{id}/repositories/{repository_id}` | account admin, `repo` scope | selected installations only (422 for `all`) |
| `GET /orgs/{org}/installations` | org admin, `read:org` | `{total_count, installations}` |

Web JSON: `GET|POST /_bgh/apps[?owner=]`, `GET|PATCH|DELETE
/_bgh/apps/{slug}`, `POST /_bgh/apps/{slug}/keys`, `DELETE
/_bgh/apps/{slug}/keys/{id}`, `GET /_bgh/apps/{slug}/install`, `POST
/_bgh/apps/{slug}/installations`, `GET /_bgh/installations[?account=]`,
`GET|PATCH|DELETE /_bgh/installations/{id}`, `PUT|DELETE
/_bgh/installations/{id}/suspended`, `POST
/_bgh/installations/{id}/accept_permissions` (permission-upgrade
acceptance: stub, notification/webhook TODO P46). Install/configure
responses carry `setup_redirect` (`setup_url?installation_id=…&setup_action=install|update`).

Audit: `integration.create|update|destroy|generate_private_key|remove_private_key`,
`integration_installation.create|destroy|suspend|unsuspend|repositories_changed|repositories_added|repositories_removed|version_update`.

## Web

* `/settings/apps[/new|/:slug]` and `/organizations/:org/settings/apps…`
  ("Developer settings"): list, register, edit (permissions per group,
  events, webhook, visibility), private keys (generate → PEM download,
  delete), delete app. `src/pages/apps/AppsManager.tsx`.
* `/apps/:slug` (public page) and `/apps/:slug/installations/new` (pick the
  account, all or selected repositories via `RepoAccessPicker`, review
  permissions, install → setup URL or installation page).
* `/settings/installations[/:id]` and `/organizations/:org/settings/installations…`
  ("GitHub Apps"): repository access, accept new permissions, suspend,
  uninstall. `src/pages/apps/InstallationsManager.tsx`.
* Mock backend `src/mock/extra/apps.ts` (+ test); unit tests
  `src/pages/apps/logic.test.ts`; Playwright smoke against a real server:
  `web/scripts/apps-smoke.mjs` (register for an org, key download, install
  on a selected repository, installations list).

## Verification

* `cargo test -p bgh-accounts --test it apps::` — full flow (register, key,
  JWT `GET /app`, install on an org with one selected repo, mint, read
  selected / 404 others, bot-authored issue, `contents:read` → 403 on
  writes, 422 narrowing errors, revoke, suspend/unsuspend), JWT errors, git
  clone/push with `x-access-token`, user endpoints, registration rules and
  permission acceptance, pagination and cross-app isolation, separate rate
  limit bucket.
* Real clients against a running server: `@octokit/auth-app` (app JWT,
  installation token via `POST …/access_tokens`, `auth.hook` routing) and
  PyGithub `GithubIntegration` (`get_app`, `get_installations`,
  `get_access_token`, `get_org_installation`, installation auth) both
  work.

## Shared-code changes (additive)

* `bgh-core`: new `apps` module; `auth::AuthMethod::App`, JWT branch in
  `token_auth`, `bghs_` accepted in Basic auth, `scopes_header` skips app
  credentials; `crypto::{INSTALLATION_TOKEN_PREFIX, new_installation_token}`;
  `perms::effective` installation branch; `ratelimit::caller_key`
  installation/app buckets; `node_id::NodeType::Integration`;
  `models::api` re-exports `Integration`, `Installation`.
* `bgh-core::token_permissions` (P8): `TokenPermissions::of` returns the
  map of installation tokens too (so the middleware, git transport and
  GraphQL guard apply to them), `classify` maps `/installation/*` to
  metadata, and reads of repositories an installation token doesn't cover
  pass through to `perms::effective` (`apps::covers_repo_named`).
  Installation tokens also store their map in `access_tokens.permissions`.
* `bgh-repos`: `apps::check_git` in `git_http::git_access` and LFS access.
* `bgh-server`: mounts `bgh_core::apps::middleware`.
* Workspace deps: `rsa` (0.9, `sha2`, `pem`; already in the lockfile) and
  `rand_core` 0.6 with `getrandom` (for `rsa::rand_core::OsRng`).
* Web: `ui/icons.ts` exports `PlugIcon`; settings nav and org settings nav
  entries; routes.

## Known gaps / TODO

* Webhook delivery, `installation` / `installation_repositories` events,
  `/app/hook/*`, manifests, user-to-server (`ghu_`) tokens and the
  permission-upgrade notification: P46.
* Organization and account permissions are stored and returned but only
  repository categories known to `token_permissions` are enforced;
  administration endpoints stay closed to installation tokens.
* `GET /app/installations?outdated` is accepted but not filtered.
