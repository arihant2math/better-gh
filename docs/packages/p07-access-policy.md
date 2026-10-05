Integration: ready
Internal visibility, private mode and allowed visibilities (backend, web, tests); gate green on the merged tree.

# P7 access-policy — status

**Done.** Branch `bgh/p07-access-policy`, merged with the latest
integration branch, full gate green (fmt, clippy, `cargo test --workspace`,
web typecheck/lint/test/build). Scope: `docs/PHASE4_PLAN.md`
§P7 (no §5 quick fixes are assigned to P7). Evidence: `docs/AUDIT.md`
("internal visibility behaves like private", "No private mode").

## What changed

### Internal visibility (GHES semantics)

* `bgh_core::perms` (additive, signatures unchanged):
  * `repo_permissions` / `repo_permission` / `RepoAccess`: internal
    repositories give **Read** to every signed-in, non-suspended *user*
    account (organizations and suspended users don't get it). Grants
    (owner, collaborator, team, org base permission, site admin) still
    raise it. Anonymous callers get nothing.
  * `users_repo_permissions` (notification/email fan-out) applies the same
    floor (`type = 'User' AND suspended_at IS NULL`).
  * `effective`: unchanged — internal is "private" for token scopes (needs
    `repo`), and Actions job tokens of *other* repositories still see only
    public repositories.
  * new `visibility_floor(repo, active_user)`, `db::Repository::is_internal()`.
  * `ReadableRepos` gains `internal: bool` (true for signed-in callers with
    the `repo` scope) and `visibility_sql(alias)`; `can_read` honours it.
* Search: `sqlb::readable` (repositories, issues, code, commits, activity),
  the command palette (`/_bgh/search`), and `GET /issues` user-centric
  filters include internal repositories for signed-in users. New
  `is:internal` qualifier (repositories, issues, code, commits);
  `is:private` keeps matching every non-public repository.
* GraphQL, sync bootstrap/partial/WebSocket scope checks, projects and
  notifications use `perms::repo_permissions`, so they follow
  automatically (tests cover sync and GraphQL). Internal repositories are
  *not* added to an outsider's default sync scope set (like public ones);
  the web client subscribes on demand (`ensureScope`).
* `GET /orgs/{org}/repos`: signed-in outsiders also see internal
  repositories; new `type=internal`.
* Forks: internal stays internal when forked into an organization and
  becomes private in a user account (`forks::visibility_for_owner`; the
  fork used to copy `internal` into user accounts). Transfers use the same
  rule (already did).
* `members_can_create_internal_repositories` (`PATCH /orgs/{org}`) is now
  stored (migration `1900_access_policy.sql`, default true), returned in
  organization-full for members and enforced for org members on create,
  fork and transfer into the org (`forks::can_create_with_visibility`).
  `members_allowed_repository_creation_type=all|private` also enables it
  unless set explicitly; `none` disables it.
* Repo JSON already rendered `private: true, visibility: "internal"`; the
  sync `repo` shape now also carries `visibility` (SYNC_PROTOCOL.md §3).

### Private mode and the anonymous directory

* New settings section `privacy` (`bgh_core::settings::PrivacySettings`):
  `private_mode` (false), `allow_anonymous_directory` (true),
  `allowed_visibilities` (all three). Editable through
  `PATCH /_bgh/admin/settings`.
* `bgh_core::privacy::private_mode_middleware`, mounted in bgh-server
  (inside maintenance mode). Anonymous requests:
  * API, GraphQL, `/_bgh/*`, raw files, archives, avatars, release
    downloads, sync WebSocket → 401 `Requires authentication`;
  * git smart HTTP / LFS without credentials → 401 + `WWW-Authenticate:
    Basic realm="Better GitHub"` (requests with credentials reach the git
    handlers, which authenticate passwords too);
  * HTML page loads → 302 `/login?return_to=<path+query>`;
  * exempt: `/healthz`, `/api/v3/meta`, `/_bgh/site`, `/_bgh/boot`,
    `/_bgh/session[/two_factor]`, `/_bgh/auth/*`, `/_bgh/signup`,
    `/_bgh/password_reset[/…]`, `/_bgh/emails/verify`, `/_bgh/sso[/…]`,
    `/login/oauth/access_token`, `/login/device/code`, `/assets/*`,
    top-level static files, and the SPA sign-in pages (`/login`,
    `/signup`, `/login/two-factor`, `/password_reset[/…]`,
    `/settings/emails/verify`);
  * raw/archive URLs with a `?token=` download token pass through (the
    handler validates it); in private mode the tarball/zipball redirect
    issues a token for every repository, not just private ones.
* Attachments (`bgh-uploads`, P6): anonymous downloads are refused by the
  middleware; signed-in downloads are never `Cache-Control: public` in
  private mode (no shared-cache replay to anonymous users).
* Container registry (`bgh-packages`, P15): `/v2/...` passes the
  middleware (Docker's Bearer challenge and registry JWTs are handled by
  the registry); in private mode the registry refuses anonymous callers
  and anonymous tokens with its challenge, and `/v2/token` issues no
  anonymous tokens.
* Second line of defence: `RepoAccess::for_repo` returns 404 to anonymous
  callers in private mode (covers any handler the middleware lets through).
* `GET /users` and `GET /organizations` require authentication in private
  mode or when `allow_anonymous_directory=false`
  (`privacy::require_directory_access`).
* `/_bgh/site` (`settings::public_info`) exposes `private_mode` and
  `repository_visibilities {allowed, default_user, default_org}`.

### Allowed visibilities

* `PrivacySettings::check_visibility` → 422 `Validation Failed` with
  `errors: [{resource: "Repository", field: "visibility", code: "custom",
  message: "<v> repositories are not allowed on this instance"}]`.
  Enforced on `POST /user/repos`, `POST /orgs/{org}/repos`,
  `PATCH /repos/{o}/{r}` (only when the visibility changes), transfer
  (resulting visibility), fork (resulting visibility) and template generate
  (`forks::check_new_repo`).
* `SiteSettings::default_visibility` falls back to the most restrictive
  allowed visibility when the configured default isn't allowed;
  `SiteSettings::validate_policy` (admin PATCH) requires a non-empty, known
  `allowed_visibilities` containing `repositories.default_visibility`.

### Web

* Repo header badge/icon: "Internal" (organization icon) vs "Private"/"Public";
  sidebar, dashboard and the repository Access settings use the same icon/text.
* `/new`: visibility options filtered by the site policy, default
  preselected from `/_bgh/site`, internal described with GHES semantics,
  `members_can_create_internal_repositories` respected.
* Repository settings → Danger zone: visibility dialog offers every other
  allowed visibility (internal for organization repositories).
* Site admin → Settings: new "Privacy" section (private mode, public user
  directory, allowed visibilities; client-side check that the default is
  allowed).
* Sign-in page: note when the instance is in private mode (from
  `/_bgh/site`).
* Mock backend: `/_bgh/site`; org JSON has the new flag.

## Migrations

* `1900_access_policy.sql`: `org_settings.members_can_create_internal_repositories`.

## Shared-code changes (all additive)

* `bgh-core`: `perms` (internal floor, `visibility_floor`,
  `ReadableRepos::{internal, visibility_sql}`), `privacy` (new module),
  `settings` (`privacy` section, `VISIBILITIES`, `validate_policy`,
  `public_info` fields, `default_visibility` fallback), `models::db`
  (`Repository::is_internal`, `OrgSettings` field), `models::api`
  (`OrganizationFull.members_can_create_internal_repositories`),
  `sync::shapes` (`repo.visibility`), `testing::TestApp::set_settings`.
* `bgh-server`: mounts the private-mode middleware.

## Tests

* `bgh-repos` `tests/it/access_policy.rs`: internal JSON/permissions
  (outsider read-only, org member, anonymous, token without `repo`,
  suspended), org repo lists, git transport (clone yes, push no, anonymous
  challenged), forks/transfers, `members_can_create_internal_repositories`,
  allowed visibilities on create/PATCH/fork/transfer/generate, private mode
  (API/GraphQL/sync/raw/archive/avatars/git 401, web 302, exempt paths,
  token and password git clones, download tokens), anonymous directory.
* `bgh-search` `tests/it/access_policy.rs`: repositories, issues, code,
  commits and palette search include internal repositories for outsiders,
  not for anonymous callers or tokens without `repo`; `is:internal`.
* `bgh-sync` `tests/it/access_policy.rs`: default scopes exclude internal,
  explicit scopes allow it read-only, token scopes, partial sync.
* `bgh-graphql` `tests/it/access_policy.rs`: `visibility: INTERNAL`,
  `viewerPermission: READ`, org repository connection, search.
* `bgh-admin` `tests/it/settings.rs::privacy_settings_are_validated`.
* `bgh-core` unit test of the private-mode path classification.
* Web: `settingsForm.test.ts` (privacy round trip and validation),
  `site.test.ts` (`visibilityPolicy`), mock `/_bgh/site`.

## Verification

* Playwright against a real server (`bgh serve` + built web client): an
  outsider sees `acme/inner` with the "Internal" badge; `/new` for an org
  lists Public/Internal/Private and only Internal/Private after the policy
  drops public (Private preselected); the admin Privacy section renders
  and flags a disallowed default inline; in private mode an anonymous
  visit to `/acme/inner` is a 302 to `/login?return_to=%2Facme%2Finner`,
  the sign-in page shows the private-mode note, and signing in returns to
  the repository.
* `scripts/gh-compat.sh --url … --token …` against a server in private
  mode: 39/40, identical to the same run with private mode off (the one
  failure, `gh pr checkout`, is specific to `--url` mode's remote setup
  and fails the same way without private mode). The default
  `scripts/gh-compat.sh` run is 40/40 and `scripts/api-smoke.sh` 45/45.

## Known gaps / notes

* Made P2's `push_hardening::push_size_limit` robust: under load git
  hangs up while the client is still uploading the oversized pack, so the
  client reports a broken pipe instead of git's message. The test now
  accepts either, and asserts the branch didn't move.

* Anonymous GraphQL outside private mode keeps working as before (GitHub
  requires auth for GraphQL; not part of P7).
* SSH needs a key, so it is never anonymous; deploy keys keep working in
  private mode.
* No org-settings UI exists yet for the `members_can_create_*` flags (API
  only).
