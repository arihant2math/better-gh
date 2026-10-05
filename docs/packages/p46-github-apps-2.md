Integration: ready
GitHub Apps part 2: app webhooks + installation events, /app/hook/*, manifest flow, user-to-server tokens with refresh, real-app check attribution, web Advanced tab / client secrets / manifest page. Re-merged with P47 (fine-grained PATs) and batch 8: auth/perms/token_permissions/crypto combine both token kinds (distinct `app:` / `fgpat:` scopes; org PAT policies don't apply to kind `app`); full gate green.

# P46 — GitHub Apps, part 2: app webhooks, installation events, manifest flow, user-to-server tokens, checks attribution

Branch `bgh/p46-github-apps-2`. Migration `5800_github_apps_part2.sql`
(range 5800–5899). Builds on P17 (`docs/packages/p17-github-apps.md`).

## What exists

* **App webhook** (`bgh_notify::webhooks::apps`). An app has one hook:
  P17's `github_apps.webhook_{active,url,secret}` plus
  `webhook_content_type` (`json`/`form`), `webhook_insecure_ssl` and
  `webhook_last_response`. Deliveries share `webhook_deliveries`
  (`hook_id` now nullable, new `app_id`; exactly one is set) and the
  `notify.deliver_webhook` job, which signs with the app's sealed secret
  and sends `X-GitHub-Hook-ID` = app id,
  `X-GitHub-Hook-Installation-Target-Type: integration`,
  `X-GitHub-Hook-Installation-Target-ID` = app id. Retries, request /
  response logging and redelivery work as for repository hooks.
* **Event routing** (`dispatch.rs` → `apps::candidates` / `queue_for`):
  every domain event whose webhook name an installation subscribed to
  (`app_installations.events`, accepted at install time) goes to the app
  hook when the installation is not suspended and covers the event's
  repository (all repositories of the account, or the selected ones) or is
  installed on the event's organization. The payload gets
  `installation: {id, node_id}`. One query for candidates, one for the
  selected-repository pairs; deliveries are keyed per app by the outbox
  event (`webhook_deliveries_app_event_idx`), so redelivered events don't
  duplicate.
* **Installation events** (`Event::AppInstallationChanged`,
  `Event::AppInstallationRepositoriesChanged`, payload builders in
  `bgh_notify::payloads::apps`), delivered only to the app's hook whatever
  it subscribed to: `installation` `created` / `deleted` (installation and
  repositories snapshotted before the row goes) / `suspend` / `unsuspend`
  (real transitions only) / `new_permissions_accepted`, and
  `installation_repositories` `added` / `removed` (selection changes in
  the web UI, including all ↔ selected, and
  `PUT|DELETE /user/installations/{id}/repositories/{rid}`). Both are
  listed as produced in P10's coverage test.
* **Permission-upgrade flow**: when an app's permissions or events change
  (`PATCH /_bgh/apps/{slug}`), administrators of every account whose
  installation differs get an email (`mail::templates::app_permissions_requested`)
  linking to the installation page, where "Accept new permissions"
  (`POST /_bgh/installations/{id}/accept_permissions`) applies them and
  emits `installation.new_permissions_accepted`.
  `GET /app/installations?outdated=true` now filters to those
  installations.
* **Manifest flow** (`bgh_accounts::apps::manifest`, table
  `github_app_manifests`): `POST /settings/apps/new[?state=]` and
  `POST /organizations/{org}/settings/apps/new` (form `manifest`; CSRF
  exempt because they only store the manifest) → 303 to the web client's
  confirmation page `…/settings/apps/new?manifest=<token>` (GET of those
  paths is served the app shell by `bgh-server`'s `spa_pages`). Confirming
  (`POST /_bgh/app-manifests/{token}` `{name?}`, session, admin of the
  owner) registers the app from `name`, `url`, `description`,
  `hook_attributes.{url,active}`, `callback_urls` (+ legacy
  `callback_url`), `setup_url`, `setup_on_update`, `public`,
  `default_permissions`, `default_events`, generates a private key, a
  client secret and a webhook secret, and returns
  `redirect_url?code=…&state=…` (or the app's settings without
  `redirect_url`). `POST /app-manifests/{code}/conversions` (no auth,
  single use, 1 hour) → 201 with the `integration` plus `client_secret`,
  `webhook_secret`, `pem`; credentials are sealed until then and wiped
  after.
* **Client secrets** (`github_app_client_secrets`, SHA-256 + last eight):
  `POST /_bgh/apps/{slug}/client_secrets` (secret shown once),
  `DELETE /_bgh/apps/{slug}/client_secrets/{id}`; listed in the app detail
  with `last_used_at`. Audited `integration.generate_client_secret` /
  `remove_client_secret`.
* **User-to-server tokens** (`bgh_accounts::apps::user_tokens`, hooked
  into `oauth.rs`): `/login/oauth/authorize` (and the web client's
  `/_bgh/oauth/authorize`) accept a GitHub App's client id (any of its
  callback URLs; consent skipped once authorized,
  `github_app_authorizations`); `POST /login/oauth/access_token` with the
  client id + a client secret supports `authorization_code` (PKCE too)
  and `refresh_token` grants and answers `access_token` (`bghu_`, 8 h),
  `expires_in`, `refresh_token` (`bghr_`, single use, 6 months),
  `refresh_token_expires_in`, `scope: ""`, `token_type`. Tokens are
  `access_tokens` rows of kind `app` owned by the user with
  `github_app_id`; scopes `app:user:{app_id}`, the repositories of the
  app's installations the user can reach (`app:owner:`/`app:repo:`) and the
  app's permission map. `perms::effective` → `apps::user_to_server_cap`:
  the user's own role capped like an installation token on covered
  repositories, anonymous view elsewhere; `token_permissions` enforces the
  categories and always allows `GET /user`, `GET /user/installations[/{id}/repositories]`
  and `PUT|DELETE /user/installations/{id}/repositories/{rid}`.
  `/user/installations` lists only the token's app. Removing a repository,
  switching to selected, suspending or uninstalling strips coverage from
  live user tokens. Basic auth accepts `bghu_` tokens.
* **Checks attribution** (`bgh-pulls/src/checks.rs`): check runs and
  suites created with an installation or user-to-server token belong to
  the real app (`check_suites.app_id`, one suite per app and commit); the
  `app` object is the app's `integration` (batch-loaded). The built-in
  Actions app renders as GitHub's (`id` 15368, slug `github-actions`),
  real app ids are kept above it (sequence bumped), and runs created by
  users with ordinary tokens have `app: null` (the fake "Better GitHub
  API" app with id 2 is gone). `?app_id=` filters on runs / suites and the
  required-check source matching (`protection::check_outcomes`, P3) use
  these ids; webhook `check_run` / `check_suite` payloads render the same
  app. The migration rewrites branch-protection checks naming the old ids
  (1 → 15368, 2 → any source).
* **P17 fix**: `POST /app/installations/{id}/access_tokens` with
  `"permissions": {}` (PyGithub's default) no longer narrows the token to
  metadata.

## Endpoints

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /app/hook/config` | app JWT | `{content_type, insecure_ssl, url, secret?: "********"}` |
| `PATCH /app/hook/config` | app JWT | `url` (SSRF-validated; `""` clears), `content_type`, `secret` (`""` clears), `insecure_ssl`; 422 on bad values |
| `GET /app/hook/deliveries` | app JWT | `hook-delivery-item[]`, `per_page`, `cursor`, `status=success|failure`, `redelivery`; `Link rel="next"` |
| `GET /app/hook/deliveries/{id}` | app JWT | `hook-delivery` with request headers/payload and response |
| `POST /app/hook/deliveries/{id}/attempts` | app JWT | 202 `{}`; 422 when the payload was pruned or no URL |
| `POST /app-manifests/{code}/conversions` | none | 201 conversion; 404 unknown / used / expired |
| `POST /settings/apps/new`, `POST /organizations/{org}/settings/apps/new` | browser form | 303 to the confirmation page; 422 text for a missing/invalid manifest |
| `POST /login/oauth/access_token` (GitHub App client) | client secret | `authorization_code`, `refresh_token` grants; GitHub's error shapes (`incorrect_client_credentials`, `bad_verification_code`, `bad_refresh_token`) |

Web JSON: `GET /_bgh/apps/{slug}/hook`, `GET /_bgh/apps/{slug}/hook/deliveries[/{id}]`,
`POST /_bgh/apps/{slug}/hook/deliveries/{id}/attempts` (app managers,
sessions only), `GET|POST /_bgh/app-manifests/{token}`,
`POST|DELETE /_bgh/apps/{slug}/client_secrets[/{id}]`; `PATCH /_bgh/apps/{slug}`
accepts `webhook_content_type` and `webhook_insecure_ssl`; app details
carry `webhook_content_type`, `webhook_insecure_ssl`, `client_secrets`.

## Tables / migrations

`5800_github_apps_part2.sql`: `github_apps.webhook_content_type`,
`webhook_insecure_ssl`, `webhook_last_response`;
`webhook_deliveries.hook_id` nullable + `app_id` (+ check, indexes, unique
per app/outbox event); `github_app_client_secrets`; `github_app_manifests`;
`access_tokens.github_app_id`; `github_app_refresh_tokens`;
`github_app_authorizations`; `check_suites.app_id`; `github_apps` id
sequence ≥ 15368; branch-protection check source rewrite.

## Web

* App settings (`src/pages/apps/AppsManager.tsx`): "General" / "Advanced"
  tabs; "Client secrets" section (generate → shown once, delete).
* `AppAdvanced.tsx` (lazy chunk): webhook delivery settings (content type,
  SSL verification, last response) and recent deliveries (filter,
  refresh, request/response headers and bodies, redeliver).
* `ManifestConfirm.tsx` (lazy chunk): `…/settings/apps/new?manifest=…`
  confirmation (owner, editable name, URLs, permissions, events) →
  redirect to the integration.
* Mock: `src/mock/extra/apps.ts` (client secrets, hook + deliveries,
  manifests with a seeded `demo` token) + tests. Initial bundle unchanged
  (143.2 KB gzip).

## Verification

* `cargo test -p bgh-notify --test it app_hooks::` — probot-style app
  receiving signed `installation` (created / suspend / unsuspend /
  new_permissions_accepted / deleted), `installation_repositories`
  (added / removed) and `issues` events with `installation`, uncovered
  repositories excluded, suspension silences events; `/app/hook/config`
  GET/PATCH (form content type + rotated secret applied), deliveries list
  / get / redeliver / `Link` / status filter, web views and 404 for
  non-managers. `coverage::` lists the two new events as produced.
* `cargo test -p bgh-accounts --test it apps_p46::` — manifest flow (org,
  personal, rename, bad manifests, non-admins, single-use code and
  confirmation, PEM authenticates), client secrets, user-to-server tokens
  (scoping, writes as the user, `contents: read` blocks writes, basic
  auth, `/user/installations`, consent skip, bad redirect, refresh +
  single-use refresh, coverage stripped on repository removal and
  suspension, deleted secret), permission-upgrade email, empty
  permission map.
* `cargo test -p bgh-pulls --test it governance::installation_token_checks_use_the_real_app`
  — an installation-token run shows the app (id, slug, name, owner,
  permissions), shares one suite per app, `?app_id=` filters, and
  satisfies a required check pinned to that `app_id` (a user's run of the
  same name doesn't). Existing tests updated for Actions = 15368 and
  `app: null`.
* Real clients against a running server (`scripts/real-clients/`):
  octokit — `apps.createFromManifest`, `get/updateWebhookConfigForApp`,
  `list/get/redeliverWebhookDelivery`, `@octokit/webhooks-methods`
  `verify()` on the received payload, `checks.create` / `listForRef`
  attribution and `check_run` webhook, `@octokit/oauth-methods`
  `exchangeWebFlowCode` + `refreshToken` (`clientType: github-app`),
  `apps.listInstallationsForAuthenticatedUser`; PyGithub —
  `GithubIntegration.get_app` / `get_installations` /
  `get_github_for_installation`, `create_check_run` attribution,
  `get_check_suites(app_id=)`, `Auth.AppUserAuth` with automatic refresh
  of an expired token, `get_user().get_installations()`.
* Playwright against a real server: `web/scripts/apps-p46-smoke.mjs`
  (integration page posts a manifest → confirmation page → redirect with
  code → conversion → client secret → install via UI → signed
  `installation` + `issues` webhooks → Advanced tab lists them, shows
  headers / response, redelivers, saves the content type).

## Shared-code changes (additive)

* `bgh-core`: `events::Event::{AppInstallationChanged,
  AppInstallationRepositoriesChanged}`; `apps::{USER_SCOPE_PREFIX,
  USER_TOKEN_TTL_SECS, REFRESH_TOKEN_TTL_SECS, user_to_server_app_id,
  user_to_server_cap}`, `AppRow` gains `webhook_content_type`,
  `webhook_insecure_ssl` (`token_covers` / `check_git` also handle user
  tokens; `effective_cap` is installation-only); `perms::effective` one
  extra branch; `token_permissions::of` covers user tokens plus
  `user_to_server_route`; `crypto::{USER_TO_SERVER_TOKEN_PREFIX,
  REFRESH_TOKEN_PREFIX, new_user_to_server_token, new_refresh_token}`;
  `auth` accepts `bghu_` in Basic auth, hides scopes of user tokens, CSRF
  exemption for `POST …/settings/apps/new`;
  `mail::templates::app_permissions_requested`.
* `bgh-server/src/web.rs` `spa_pages`: GET of `…/settings/apps/new`
  serves the shell (the path is a POST-only backend route).
* `bgh-pulls`: `checks::{ACTIONS_APP_ID, caller_app, suite_app_id}`,
  `ensure_suite` / `create_run` take `(slug, app_id)`;
  `checks::AppJson` is now `bgh_core::apps::Integration`.

## Known gaps / TODO

* The device flow is not offered for GitHub App client ids, and
  `/applications/{client_id}/token` (check / reset / delete) is OAuth-app
  only; users can't yet revoke an app authorization from settings
  (tokens expire after 8 h; uninstalling / suspending strips coverage).
* User-to-server tokens use the app's permissions, not the intersection
  with each installation's accepted permissions.
* An app may update other apps' check runs on repositories it covers
  (GitHub limits updates to the creating app).
* GraphQL `CheckSuite.app` still renders from the suite slug.
* `check_suite` `requested` events are not auto-created for apps on push.
