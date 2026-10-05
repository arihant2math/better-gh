Integration: landed
P36 account security: WebAuthn security keys + passkeys, org/site 2FA requirements, sudo mode, encrypted TOTP secrets, PAT expiry mails and header; backend, web UI, tests.

# P36 — Account security

Branch `bgh/p36-account-security`. Scope: `docs/PHASE4_PLAN.md` §P36.

## Implemented

### WebAuthn (`bgh-accounts/src/webauthn.rs`, `webauthn-rs` 0.5)

Relying party = host of `BGH_BASE_URL` (a loopback IP becomes `localhost`,
since browsers reject IP RP IDs), origin = base URL. Ceremony state lives in
Redis (`webauthn:{purpose}:{sha256(id)}`, 5 min, single use via `GETDEL`,
bound to the user and to the pending login / session).

| Endpoint | Notes |
|---|---|
| `GET /_bgh/user/webauthn` | `[{id, name, kind, created_at, last_used_at}]` (session only) |
| `POST /_bgh/user/webauthn/registrations {kind}` | 201 `{id, options}`; `kind` = `security_key` (needs TOTP enabled, else 422) or `passkey` (resident key required, UV required); **sudo** |
| `POST /_bgh/user/webauthn/registrations/{id} {name, credential}` | 201 credential; 422 on bad attestation / duplicate; **sudo** |
| `PATCH /_bgh/user/webauthn/{id} {name}` · `DELETE …` | rename (1–64 chars) · delete (**sudo**) |
| `POST /_bgh/auth/login/passkey/challenge` → `POST /_bgh/auth/login/passkey {id, credential}` | passwordless sign-in (discoverable credentials) → boot JSON + cookie |
| `POST /_bgh/auth/2fa/webauthn/challenge {twoFactorToken}` → `POST /_bgh/auth/2fa/webauthn {twoFactorToken, id, credential}` | security key (or passkey) as the second factor of a password sign-in; counts toward the 5 pending-login attempts |

`POST /_bgh/auth/login`'s 401 now carries `twoFactorMethods` (`totp`,
`recovery_code`, plus `webauthn` when keys exist). Recovery codes still work
everywhere. Disabling TOTP deletes security keys (passkeys stay). Sign
counters / backup state are updated after each assertion.

### Sudo mode (`bgh_core::sudo`, `bgh-accounts/src/security.rs`)

`sessions.sudo_at` (new sessions start in sudo mode: signing in is a fresh
authentication; pre-existing sessions have none). Valid 2 h.
`bgh_core::sudo::require(&state, &auth)` → 401 `SUDO_REQUIRED` ("Sudo mode
required: …") for cookie sessions without it; tokens pass. Applied to: PAT
creation (`/_bgh/tokens`), SSH and GPG key creation, email add / remove /
set primary, OAuth app creation and secret regeneration, GitHub App
creation and private-key generation, WebAuthn registration and deletion,
repository deletion and transfer (bgh-repos, one line each).

`GET /_bgh/sudo` → `{active, expires_at, methods: {password, totp, webauthn}}`;
`POST /_bgh/sudo/webauthn/challenge` → `{id, options}`;
`POST /_bgh/sudo {password | otp | webauthn: {id, credential}}` → status (403
wrong, 422 empty, 429 after 10 failures / 15 min). Audit `user.sudo`.

### Two-factor requirements

* **Org** (`org_two_factor.rs`): `PATCH /orgs/{org}` accepts
  `two_factor_requirement_enabled` (422 field error unless the owner has
  2FA). Turning it on removes every member and outside collaborator without
  2FA (team memberships, direct repo grants, pending repo invitations; sync
  + `AccessChanged`), records them in `org_two_factor_removals`, mails them
  (`mail::templates::org_two_factor_removed`) and audits
  `org.remove_two_factor_non_compliant`. Invitations to users without 2FA →
  422; accepting an invitation without 2FA → 403. Rejoining within 3 months
  reinstates the recorded teams and direct repo grants. Users who belong to
  (or collaborate with) a 2FA org can't disable 2FA (422).
* **Site** (`auth_providers.require_2fa`, `bgh_core::settings`): the admin
  enabling it must have 2FA (422). Boot JSON flags
  `user.twoFactorSetupRequired`; `security::require_two_factor_middleware`
  (mounted in bgh-server) answers 403 to such cookie sessions except the 2FA
  setup / auth / sudo endpoints, `GET /api/v3/user` and private GETs; tokens
  are unaffected. 2FA can't be disabled while required. Exposed in
  `GET /_bgh/site` (`require_2fa`).

### TOTP encryption at rest

`user_two_factor.totp_secret_enc` (XChaCha20-Poly1305 with the server key,
`bgh_core::secretbox`); new secrets are stored only encrypted. Legacy
plaintext rows are encrypted by the `accounts.security` service at start-up
(`security::encrypt_legacy_totp`) and lazily on first use.

### PAT expiry

* `GitHub-Authentication-Token-Expiration: YYYY-MM-DD HH:MM:SS UTC` on
  responses to requests authenticated with an expiring token (task-local
  set in `auth::token_auth`, emitted by `auth_headers_middleware`).
* Reminder mails 7 days and 1 day before expiry
  (`security::send_expiry_reminders`, hourly in the `accounts.security`
  service under a pg advisory lock; `access_tokens.expiry_notified_*_at`
  make each one single-shot). Template `mail::templates::token_expiring`.

### Web

* `api/webauthn.ts`: base64url + option/credential JSON conversion (no
  dependency), ceremonies, endpoints. `api/client.ts`: a sudo 401 calls the
  installed handler (`setSudoHandler`) and retries once.
* Lazy `app/sudoPrompt.tsx` + `SudoDialog.tsx` ("Confirm access":
  password, authentication code, or security key / passkey), installed by
  `App` as the client's sudo handler. Initial JS grows by 0.4 KB gzip (the
  retry hook and the 2FA-setup redirect); everything else is lazy.
* Settings → Password and authentication: Passkeys and Security keys
  sections (add / rename / delete), site-requirement banner; the app keeps
  users with `twoFactorSetupRequired` on `/settings/security`.
* Login: "Sign in with a passkey"; 2FA step offers "Use security key".
* Org settings → Authentication security (`/organizations/:org/settings/security`):
  requirement switch, confirmation listing the members / collaborators that
  will be removed.
* Site admin → Authentication: "Require two-factor authentication".
* Mock backend: `/_bgh/user/webauthn` (list/rename/delete), `/_bgh/sudo`,
  2FA status counts (the mock can't run ceremonies, so the passkey buttons
  are hidden in mock mode).

## Migrations

`migrations/4800_account_security.sql`: `user_two_factor.totp_secret_enc`
(+ plaintext column nullable, check constraint), `sessions.sudo_at`,
`user_webauthn_handles`, `user_webauthn_credentials`,
`org_two_factor_removals`, `access_tokens.expiry_notified_{7d,1d}_at` and
an index for expiring PATs.

## Shared-code changes (additive)

* `bgh-core`: new `sudo.rs`; `settings::AuthProviderSettings.require_2fa`
  (+ `public_info`); `mail::templates::{token_expiring,
  org_two_factor_removed}`; `auth.rs`: a task-local token expiry and the
  header in `auth_headers_middleware`, `token_expiration_header` (no
  signature changes; P14 edits stay mergeable).
* bgh-server: one middleware layer (`require_two_factor_middleware`).
* bgh-admin: `require_2fa` validation in `PATCH /_bgh/admin/settings`.
* bgh-repos: `sudo::require` in `delete_repo` and `transfer`.
* Workspace deps: `webauthn-rs` (features `danger-allow-state-serialisation`,
  `danger-credential-internals`, `conditional-ui`), `webauthn-rs-proto`,
  dev `webauthn-authenticator-rs` (`softpasskey`). **webauthn-rs links
  system OpenSSL**: the Dockerfile installs `pkg-config libssl-dev` in the
  build stage and `libssl3` in the runtime image; `docs/SELF_HOSTING.md`
  notes the build dependency.
* `session.rs`: `pending_login` / `PendingLogin::{count_attempt, complete}`
  split out of `verify_pending_two_factor` (same behaviour); `boot.rs`:
  `signed_in` is public, `BootUser.two_factor_setup_required`.

## Gate

Merged `origin/claude/sleepy-cray-9jj0t3` once; `cargo fmt --check`,
`cargo clippy --workspace --all-targets -D warnings`, `cargo test
--workspace` (all green), web typecheck / lint / test (427) / build
(144.3 KB gzip initial) green; `scripts/passkey-smoke.mjs` green.

## Tests

* `crates/bgh-accounts/tests/it/security.rs` (software authenticator from
  `webauthn-authenticator-rs`): security key registration + 2FA sign-in +
  replay/cross-login rejection + rename/delete; passkey passwordless
  sign-in (+ unknown handle); sudo mode on PAT / SSH key / email / repo
  delete / OAuth app, renewal with password, TOTP and WebAuthn; no
  plaintext TOTP secrets (new, legacy pass, lazy); org requirement removal,
  invite refusal, reinstatement; expiry mails + header; site requirement.
* Web: `api/webauthn.test.ts` (conversion, sudo retry), mock tests.
* `web/scripts/passkey-smoke.mjs`: real backend + Chromium virtual
  authenticator (CDP): password sign-in, passkey registration, sudo prompt
  on token generation, passwordless sign-in, sudo via passkey. Passes.

## Known gaps

* Outside collaborators removed by the org requirement are reinstated only
  if they rejoin as members (repo re-invitations don't consult the record).
* Repository collaborator invitations (bgh-repos) don't check the org 2FA
  requirement yet.
* Security-key-only 2FA (without TOTP) isn't offered, like GitHub requires
  an app/SMS method first.
* The user-facing security log (P64) doesn't list the new audit actions
  yet: `user.sudo`, `passkey.register/remove`,
  `two_factor_authentication.{add,remove}_security_key|webauthn_used`,
  `org.{enable,disable}_two_factor_requirement`.
* `DELETE /user` (P50) should call `bgh_core::sudo::require`.
