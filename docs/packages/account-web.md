# Package F1: account-web

Branch `bgh/account-web` · web client (`web/`) plus one small bgh-server
change. Merged with the integration branch.

## Status

Scope complete. Every page works against the real backend and in mock mode
(`?mock`). `npm run typecheck && npm run lint && npm test && npm run build`
pass. Initial JS is about 124 KB gzip; every page is its own lazy chunk, and
the largest F1 chunk is under 20 KB gzip.

## Pages

| Route | Chunk | Backend |
|-------|-------|---------|
| `/login`, `/signup` | `pages/auth/{Login,Signup}Page` | `POST /_bgh/auth/{login,2fa,signup}`, `GET /_bgh/sso` |
| `/login/two-factor?token=` (SSO + 2FA) | `TwoFactorPage` | `POST /_bgh/session/two_factor`, `GET /_bgh/boot` |
| `/password_reset[/:token]` | `PasswordResetPage` | `POST /_bgh/password_reset`, `GET\|POST /_bgh/password_reset/{token}` |
| `/settings/emails/verify?token=` | `VerifyEmailPage` | `POST /_bgh/emails/verify` |
| `/login/device` (`gh auth login --web`) | `DevicePage` | `GET /_bgh/device/{code}`, `POST /_bgh/device` |
| `/login/oauth/authorize?…` (consent) | `OAuthAuthorizePage` | `GET\|POST /_bgh/oauth/authorize` |
| `/settings/profile` | avatar crop (canvas) + upload | `PATCH /user`, `PUT\|DELETE /_bgh/user/avatar` |
| `/settings/account` | SSO identities | `GET\|DELETE /_bgh/user/identities` |
| `/settings/appearance` | theme + density (local) | — |
| `/settings/notifications` | per-reason web/email | `GET\|PUT /_bgh/notifications/settings` |
| `/settings/emails` | add/remove/primary/resend/visibility | `/user/emails`, `/_bgh/user/emails/{e}/…`, `/user/email/visibility` |
| `/settings/security` | password, TOTP 2FA (client-side QR SVG, `pages/settings/qr.ts`), recovery codes | `/_bgh/user/password`, `/_bgh/user/two_factor…` |
| `/settings/sessions` | list/revoke | `/_bgh/sessions[/{id}]` |
| `/settings/keys` | SSH + GPG | `/user/keys`, `/user/gpg_keys` |
| `/settings/blocked` | block/unblock | `/user/blocks/{u}` |
| `/settings/applications` | authorized OAuth apps | `/_bgh/authorizations[/{id}]` |
| `/settings/developers[/new\|/{id}]` | OAuth apps, secret regen | `/_bgh/applications…` |
| `/settings/tokens[/new]` | PATs with scope tree + expiry, show-once | `/_bgh/tokens[/{id}]` |
| `/settings/local` | local DB / sync stats | — |
| `/{user}` (`?tab=repositories\|stars\|followers\|following`) | `pages/profile` | `/users/{u}…`, `/user/following/{u}` |
| `/{org}` (`?tab=repositories\|people\|teams`) | `pages/profile` | `/orgs/{org}…` |
| `/new` | owner picker, availability, visibility, template, README | `POST /user/repos`, `/orgs/{org}/repos`, `/repos/{t}/generate` |
| `/organizations/new` | | `POST /_bgh/orgs` |
| `/{o}/{r}/settings[/access\|branches\|branch_protection_rules/…\|keys\|hooks[/{id}]\|key_links]` | `pages/repo-settings` (one chunk per section) | repos-api + notify hooks + teams |

Shared pieces: `web/src/components/settings/kit.tsx` (sections, lists,
toggles, radio cards, typed-confirmation dialog, 422 → field errors,
`useAction`), `web/src/api/scopes.ts` (OAuth scope descriptions),
`SettingsLayout` (persistent nav). Command palette has "Settings: …",
"Create new repository/organization"; the top bar "+" menu links to both.

## Mock mode

`MockServer.route(method, pattern, handler, {public, override})` lets areas
register handlers in `web/src/mock/extra/{auth,user,developer,profile,repo}.ts`
(state per server via `extra/util.ts`). Handy mock values: login password
`2fa` (code `123456`), `throttle` (429), `wrong` (422); reset token
`valid-token`; email token `valid`; device code `ABCD-1234`; OAuth
`client_id=unknown` → 422.

## Smoke tests

`web/scripts/smoke-{auth,user-settings,developer-settings,profiles,repo-settings}.mjs`
(mock mode, `node scripts/smoke-X.mjs http://localhost:PORT outDir`) and
`web/scripts/smoke-real.mjs` (against a real server).

## Shared-code changes

- bgh-server `web.rs`: `WebFiles::has_shell`, `spa_pages` middleware. `GET`
  of `/login/device` and `/login/oauth/authorize` with `Accept: text/html`
  gets the app shell when a web client is built; other clients and POSTs
  still reach bgh-accounts' server-rendered fallback (test in
  `tests/server.rs`).
- web: `App.tsx` renders shell-less ("bare") auth pages; `routes.ts` has one
  route per settings section; `theme.ts` gained `density` (`html[data-density]`,
  applied pre-paint in `index.html`); `tokens.css` compact density and
  `[data-theme-scope]` palettes; `session.ts` gained `adopt`, `refreshBoot`,
  `completeTwoFactor`, `updateUser`; vite proxies `/avatars` and the OAuth
  token/device-code endpoints.

## Backend gaps (found while building)

- No self-service username change or account deletion (only
  `/admin/users/{u}`); the UI shows both disabled with an explanation.
- No events API → profile "Contribution activity" hidden.
- `create.rs` ignores `gitignore_template` / `license_template` (UI marks
  them unsupported and doesn't send them); invalid names are rejected
  rather than normalized (client normalizes first).
- `PATCH /repos` allows disabling every merge method (client blocks it).
- No `/search/users`; collaborator picker uses local users + exact lookup.
- Classic branch protection is per concrete branch (wildcards need rulesets).
- Authorizations JSON has no owner / last-used; SSH keys have no
  fingerprint (computed client-side); team JSON has no member count; org
  members list has no role; boot data has no `siteAdmin`.
- `repo_sync_json` lacks `topics`, `has_projects`, `has_wiki`.
- PATCH /user has no pronouns; no endpoint to link a new SSO identity while
  signed in.
