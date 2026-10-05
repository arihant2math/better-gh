# Package B1: accounts (`bgh-accounts`)

Branch `bgh/accounts`. Migrations: `0100_accounts.sql`.

## Status

The full B1 scope is implemented and covered by integration tests
(`cargo test -p bgh-accounts`: unit tests plus 7 integration test files,
about 50 tests). Every endpoint has tests that check the GitHub JSON shape
and the permission rules (scopes, owner/member/outsider, 404 versus 403).

## Endpoints

REST paths are relative to `/api/v3`.

**Users**
`GET|PATCH /user`, `GET /user/{account_id}`, `GET /users` (Link with
`since`), `GET /users/{username}`.

**Emails**
`GET|POST|DELETE /user/emails`, `GET /user/public_emails`,
`PATCH /user/email/visibility`.
Web client: `POST /_bgh/emails/verify {token}`,
`POST /_bgh/user/emails/{email}/verification` (resend),
`PUT /_bgh/user/emails/{email}/primary`. New addresses are unverified until
the mailed link is opened (link target `/settings/emails/verify?token=`).

**Followers**
`GET /user/followers|following`, `GET|PUT|DELETE /user/following/{u}`,
`GET /users/{u}/followers|following`, `GET /users/{u}/following/{target}`.

**Blocks**
`GET /user/blocks`, `GET|PUT|DELETE /user/blocks/{u}`. Blocking removes
follows in both directions and prevents following and org invitations.

**SSH keys**
`GET|POST /user/keys`, `GET|DELETE /user/keys/{id}`, `GET /users/{u}/keys`.
Keys are fully validated: ed25519, rsa (at least 1024 bits),
ecdsa-p256/384/521, and the sk-* variants. Fingerprints are
`SHA256:...` and unique across user and deploy keys.

**GPG keys**
`GET|POST /user/gpg_keys`, `GET|DELETE /user/gpg_keys/{id}`,
`GET /users/{u}/gpg_keys`. The in-house OpenPGP parser (`gpg.rs`) handles
v4, v5 and v6 keys. Subkeys are stored as rows with `primary_key_id` set.
Each email gets `verified: true` when it matches one of the user's verified
addresses.

**Web client boot + auth** (docs/SYNC_PROTOCOL.md §9-10)
- `GET /_bgh/boot` returns `{user, csrf, config, ts}`.
- `POST /_bgh/auth/login`: 200 with boot and a cookie; 422 on bad
  credentials; 401 `{twoFactorRequired, twoFactorToken}` for accounts with
  2FA.
- `POST /_bgh/auth/2fa {twoFactorToken, code}`, `POST /_bgh/auth/signup`
  (201), `POST /_bgh/auth/logout` (204, emits `Event::SessionEnded`).
- bgh-server injects the boot `<script>` (escaped) at `<!--BGH_BOOT-->`
  in the SPA shell (`/`, `/index.html` and unknown paths), with
  `Cache-Control: no-cache, private`.
- CSRF: cookie-authenticated mutations need
  `X-CSRF-Token = csrf_token(cookie)`, otherwise 403
  (`bgh_core::auth::csrf_middleware`). Token-authenticated requests are
  exempt, as are sign-in endpoints and the OAuth forms, which use their own
  nonces.
- The web login page handles the 2FA code step.

**Sign up, login and sessions** (older JSON endpoints, kept as aliases)
`POST /_bgh/signup`; `POST /_bgh/session {login, password, otp?}` returns
200 with a cookie, or 202 `{two_factor_required, two_factor_token}` (with
`X-GitHub-OTP: required; app`) for accounts with 2FA; then
`POST /_bgh/session/two_factor {two_factor_token, code}`;
`DELETE /_bgh/session`. Session management: `GET /_bgh/sessions` (with
`current`), `DELETE /_bgh/sessions/{id}`, and `DELETE /_bgh/sessions`
(revokes all others).

**Passwords**
`PUT /_bgh/user/password {current_password, password}` signs out other
sessions and sends a notification mail.
`POST /_bgh/password_reset {email|login}` always returns 202 and mails the
link `/password_reset/{token}`, valid 1 h. `GET /_bgh/password_reset/{token}`
returns `{login, two_factor_required}`.
`POST /_bgh/password_reset/{token} {password, otp?}` resets the password and
signs out every session. The OTP is required when 2FA is enabled.

**TOTP 2FA**
`GET /_bgh/user/two_factor`, `POST /_bgh/user/two_factor/totp` (returns the
secret and `otpauth://` URI), `POST /_bgh/user/two_factor/totp/enable
{code}` (returns 10 recovery codes), `DELETE /_bgh/user/two_factor
{password}`, `POST /_bgh/user/two_factor/recovery_codes {password}`.
TOTP replay is rejected via `last_used_step`. Recovery codes are single use.
Accounts with 2FA can't use Basic password auth (git) and must use tokens.
The private-user field `two_factor_authentication` reflects the state.

**PATs**
`GET|POST /_bgh/tokens`, `DELETE /_bgh/tokens/{id}`. These use classic
scopes and an optional `expires_in_days`, and require a session. The core
returns `X-OAuth-Scopes`.

**OAuth apps**
- App management (session only): `GET|POST /_bgh/applications`,
  `GET|PATCH|DELETE /_bgh/applications/{id}`,
  `POST /_bgh/applications/{id}/client_secret`.
- Grants: `GET /_bgh/authorizations`, `DELETE /_bgh/authorizations/{id}`.
  Revoking a grant revokes the app's tokens.
- Authorization code flow with PKCE: `GET /login/oauth/authorize` serves a
  server-rendered consent page, or redirects straight away when the scopes
  were already granted. Anonymous visitors are sent to `/login?return_to=`.
  `POST /login/oauth/authorize` receives the consent form.
- Web-client consent: `GET|POST /_bgh/oauth/authorize`.
- Token endpoint: `POST /login/oauth/access_token` handles the code and
  device grants. Responses are form-encoded unless `Accept: json`, errors
  come back as 200 with `error`, and client credentials may be sent as Basic.
- Device flow: `POST /login/device/code`, `GET|POST /login/device` (HTML
  code-entry page with CSRF nonce), and for the web client
  `GET /_bgh/device/{user_code}` and `POST /_bgh/device`. Polls return
  `authorization_pending`, `slow_down` (the interval grows by 5 s),
  `access_denied` or `expired_token`.
- Built-in app: the `gh` CLI client id `178c6fc778ccc68e1d6a`, seeded by the
  migration as a public client with device flow enabled. Tokens are `bgho_…`
  and are also accepted as Basic passwords.
- GitHub's app API (Basic `client_id:client_secret`):
  `POST|PATCH|DELETE /applications/{client_id}/token` and
  `DELETE /applications/{client_id}/grant`.

**OIDC SSO**
`GET /_bgh/sso`, `GET /_bgh/sso/{id}/login?return_to=`,
`GET /_bgh/sso/{id}/callback`, `GET /_bgh/user/identities`,
`DELETE /_bgh/user/identities/{id}`.
- Configuration comes from `site_settings['auth.oidc']` (an object or an
  array), or from the `BGH_OIDC_*` env variables for a single provider; see
  the `sso.rs` docs.
- The flow uses code + PKCE + nonce, with discovery cached in Redis.
- Identities are linked by (provider, sub), then by verified email.
  `auto_create` and `allowed_domains` are honoured.
- Accounts with 2FA are redirected to `/login/two-factor?token=…&return_to=`
  (the SPA page posts to `/_bgh/session/two_factor`).

**Organizations**
- Profile and settings: `GET|PATCH /orgs/{org}`, `GET /organizations`
  (Link with `since`), `GET /user/orgs`, `GET /users/{u}/orgs` (public
  memberships only, or all of them for the user themself).
  `POST /admin/organizations` is for site admins; `POST /_bgh/orgs` lets any
  user create an org.
- Members: `GET /orgs/{org}/members` (`filter=2fa_disabled`, `role`),
  `GET|DELETE /orgs/{org}/members/{u}`, `GET /orgs/{org}/public_members`,
  `GET|PUT|DELETE /orgs/{org}/public_members/{u}`.
- Memberships: `GET|PUT|DELETE /orgs/{org}/memberships/{u}`. PUT for a
  non-member creates a pending invitation. `GET /user/memberships/orgs`,
  `GET|PATCH /user/memberships/orgs/{org}` (PATCH accepts the invitation).
- Invitations: `GET|POST /orgs/{org}/invitations`,
  `DELETE /orgs/{org}/invitations/{id}`,
  `GET /orgs/{org}/invitations/{id}/teams` (also under
  `/organizations/{id}/…`), `GET /orgs/{org}/failed_invitations`. Email
  invitations to an address that is a user's verified email invite that
  user. Invitation mails are sent.
- Outside collaborators: `GET /orgs/{org}/outside_collaborators`,
  `PUT|DELETE /orgs/{org}/outside_collaborators/{u}`. Converting a member
  turns their team grants into direct collaborator grants.
- Blocks: `GET /orgs/{org}/blocks`, `GET|PUT|DELETE /orgs/{org}/blocks/{u}`.
- Org settings include `default_repository_permission` (enforced by
  `bgh_core::perms`), the repository-creation flags, and the new
  `members_can_create_teams` and `web_commit_signoff_required`.
- The last owner can't be removed or demoted.

**Teams**
- Endpoints: `GET|POST /orgs/{org}/teams`, `GET /user/teams` (team-full),
  and per team `GET|PATCH|DELETE`, `/teams` (children), `/members?role`,
  `/memberships/{u}`, `/invitations`, `/repos`, `/repos/{owner}/{repo}`.
  `Accept: …repository+json` returns team-repository; otherwise the
  response is 204.
- Every team route answers under `/orgs/{org}/teams/{slug}`,
  `/organizations/{org_id}/team/{team_id}` (the form used in `url`), and the
  legacy `/teams/{id}`.
- Nesting: parents must be closed, secret teams can't be nested, cycles are
  rejected, and deleting a team cascades to its child teams.
- Secret teams are visible only to owners and their own members. Owners and
  maintainers manage teams; members can leave.
- Adding a repository requires managing the team plus repo admin rights
  (owners always qualify). The repo must belong to the org.
- Members of child teams count as members of the parent. Team grants are
  inherited through `perms` (already in core).

**Avatars**
`GET /avatars/u/{id}?s=` serves the uploaded image or a deterministic 5×5
identicon PNG. Responses carry an ETag and return 304 on a match;
`max-age=86400`, and versioned uploads are cached as immutable.
`PUT|DELETE /_bgh/user/avatar` takes a raw PNG, JPEG, GIF or WebP body of
at most 1 MiB. `PUT|DELETE /_bgh/orgs/{org}/avatar` is for owners. An upload
sets `avatar_url` to `/avatars/u/{id}?v={sha}`.

**Rate limits**
`GET /rate_limit` (not counted against the limit) and `GET /api/v3/` (API
root; `gh auth login --with-token` reads `X-OAuth-Scopes` here).

## Tables (0100_accounts.sql)

- New tables: `account_tokens` (password reset and email verification),
  `user_two_factor`, `user_recovery_codes`, `user_blocks` (users and orgs as
  blockers), `oauth_apps` (seeded with the gh CLI app),
  `oauth_authorizations`, `user_identities`, and `user_avatars` (BYTEA).
- Altered tables: `org_members.id` (identity column used by the sync
  `membership` model), `org_settings.members_can_create_teams` and
  `.web_commit_signoff_required`, `org_invitations.failed_reason` plus
  pending-uniqueness indexes, and `access_tokens.oauth_app_id`.
- Added indexes on teams, follows and users.
- Ephemeral state lives in Redis: OAuth codes, device codes, consent and
  CSRF nonces, pending 2FA logins, OIDC state, and throttle counters.

## Sync

The models follow docs/SYNC_PROTOCOL.md and live in the scope `org:{id}`:

- `org`: on create and on PATCH.
- `membership`: id is `org_members.id`; on add, role change and removal.
- `team`: the full row with `memberIds` and `repoIds` on every change; on
  delete the payload is `{id}`.

Profile changes record `user` in `user:{id}` and in each org scope.
`create_org` was switched from the old `organization` and `org_member`
model names to these.

## Shared-code changes (bgh-core / bgh-server, all additive)

- `bgh_core::mail` (new): `Message`, `send`, `outbox`. It uses SMTP via
  lettre with rustls when `BGH_SMTP_URL` is set; otherwise mail is logged
  and written to `{data_dir}/mail/*.eml|json` (tests read the outbox). The
  job `accounts.send_mail` delivers queued mail. B5 can reuse
  `bgh_core::mail::send`.
- `bgh_core::ratelimit` (new):
  - API middleware with `X-RateLimit-*` headers and 403 when exceeded (with
    `Retry-After`).
  - Uses Redis fixed windows per user or per IP. The `core` resource allows
    5000/h authenticated and 60/h anonymous; `search` allows 30/min and
    10/min.
  - Fails open if Redis is down, and does not count `/rate_limit`.
  - Helpers `hit`, `count` and `clear` for throttling.
  - Mounted on `/api/v3` in `bgh-server/src/lib.rs`.
- `Config`: `smtp_url` (`BGH_SMTP_URL`), `mail_from` (`BGH_MAIL_FROM`),
  `rate_limit_authenticated` (`BGH_RATE_LIMIT`, 0 disables),
  `rate_limit_anonymous` (`BGH_RATE_LIMIT_ANONYMOUS`), `trust_proxy`
  (`BGH_TRUST_PROXY`), and `Config::mail_from()`. The test harness sets the
  anonymous limit to 5000, because in-process requests share one
  "unknown" IP.
- `auth`:
  - `resolve_request` lets middleware resolve auth and cache it for the
    extractors.
  - `client_ip` reads `ConnectInfo`, or the XFF/X-Real-IP headers when
    `trust_proxy` is set; `main.rs` now serves with connect info.
  - Basic auth accepts `bgho_` OAuth tokens.
  - Basic *password* auth is refused for 2FA accounts. This queries
    `user_two_factor` from migration 0100.
- `crypto`: `OAUTH_TOKEN_PREFIX`, `new_oauth_token`, `constant_time_eq`.
- `urls::avatar`: custom values starting with `/` are made absolute.
- `events`: `OrgMemberRemoved`, `OrgMemberInvited`, `TeamCreated`,
  `TeamEdited`, `TeamDeleted`, `TeamMemberAdded`, `TeamMemberRemoved`,
  `TeamRepoAdded`, `TeamRepoRemoved`, `UserFollowed`.
- `node_id::NodeType::OrganizationInvitation`.
- `api::OrganizationFull` gains member-only fields:
  `members_can_create_public_repositories`, `…_private_…`,
  `members_can_fork_private_repositories`, and
  `members_allowed_repository_creation_type`.
- `auth::csrf_token`, `auth::csrf_middleware` (layered in bgh-server), and
  `Event::SessionEnded {user_id, session_id}`, which bgh-sync should
  consume to close sockets with 4001. `TestRequest::cookie` adds the
  matching `X-CSRF-Token` automatically; use `.header("cookie", …)` to test
  rejection.
- bgh-server `web.rs`: shell injection via
  `bgh_accounts::boot::{boot_json, boot_script}`; `EmbeddedFiles::contains`
  and `EmbeddedFiles::read`.
- `bgh_accounts::users::insert_user` (pub) creates users with an optional
  password hash, as SSO needs.
- Workspace dependencies: lettre, reqwest (rustls), hmac, sha1, flate2,
  crc32fast, url.

## Known gaps / TODO

- The `gh` login also needs GraphQL `viewer { login }` (owned by B9) for the
  username after `gh auth login`.
- The HTML pages for `/login/oauth/authorize` and `/login/device` are
  minimal server-rendered fallbacks. The SPA (F1) can use the
  `/_bgh/oauth/authorize` and `/_bgh/device` JSON endpoints instead and must
  provide `/login`, `/login/two-factor`, `/password_reset/{token}` and
  `/settings/emails/verify`.
- TOTP secrets are stored in plaintext; there is no server secret key to
  encrypt them yet.
- Org-level enforcement of `two_factor_requirement_enabled` is not
  implemented. The `DELETE /orgs/{org}` endpoint is left to admin (B7),
  because it needs repo storage cleanup.
- `GET /api/v3/` (trailing slash) is mounted as an absolute web route, so it
  has no rate-limit headers.
