Integration: ready
LDAP sign-in and sync, password_login enforcement, git basic-auth throttling, LDAP/OIDC team sync, admin LDAP UI.

# P14 ldap-auth — status

**Done.** Branch `bgh/p14-ldap-auth`, merged with the latest integration
branch and gate-green (fmt, clippy, `cargo test --workspace`, web
typecheck/lint/test/build; earlier also `api-smoke.sh` 45/45 and
`gh-compat.sh` 41/41). Landing is up to the integrator (WORKER_GUIDE rule
14). Scope: `docs/PHASE4_PLAN.md` §P14 (no §5 quick fixes are assigned to
P14). Migrations: 2600–2699 (`2600_directory_auth.sql`).

## What changed

### One password policy (`bgh_core::auth::check_password`)

Web sign-in (`POST /_bgh/session`, `/_bgh/auth/login`) and git/LFS/wiki
HTTP basic auth with a password (`AuthOptions { allow_password: true }`)
now share one function:

1. Throttle: locked (429 "Too many failed login attempts…") after 10
   failures of the login or 50 of the client IP within 15 min. The
   counters (`login_fail:{login}`, `login_fail_ip:{ip}`) are shared by web
   and git, so 20 bad git passwords lock the account for web sign-in too.
2. The password directory (LDAP) is asked first when one is installed and
   enabled (`auth::PasswordDirectory`, registered by
   `bgh_accounts::register` via `auth::set_password_directory`).
3. Built-in passwords: refused with 403 when
   `auth_providers.password_login` is false ("Password sign-in is
   disabled…" / for git "Password authentication is disabled… Use a
   personal access token"), decided *before* the password is checked (no
   oracle, no failure counted). Site admins are exempt only with the new
   `auth_providers.password_login_admin_exempt` (break-glass). PATs and
   OAuth tokens always work (basic auth with a token never reaches this).
4. Failures count against both throttles and write `user.failed_login`
   audit rows with `{login, transport: "web" | "git"}` and the client IP.
   Git requests get the IP through a task-local peer address set by
   `auth_headers_middleware` (`auth::request_ip`), so no git handler
   signature changed.
5. Suspended → 403 (audited). Accounts with 2FA still can't use passwords
   for git.

The REST API still never accepts passwords (like GitHub); "API basic"
in the plan is the `_bgh` session API, which goes through the same path.

### LDAP (`bgh_accounts::ldap`, crate `ldap3` with rustls)

Settings `auth_providers.ldap` (`bgh_core::settings::LdapSettings`): host,
port, `encryption` (`none|ldaps|starttls`), `ca_cert` (PEM),
`verify_certificate`, `bind_dn` / `bind_password` (write-only, redacted
like other secrets), `user_search_bases`, `uid_field`, `user_filter`,
`admin_group`, `restricted_group`, attribute mapping (`name_field`,
`email_field`, `ssh_key_field`, `gpg_key_field`), `jit_provisioning`,
`sync_enabled`, `sync_interval_hours`. Validated by the admin settings API;
"at least one sign-in method" now counts LDAP.

* **Sign-in**: service bind → search `(&({uid}={login}){user_filter})`
  under each base → bind as the entry (separate connection; empty
  passwords refused) → restricted group → find the linked account
  (`user_identities` provider `ldap`, subject = normalized DN), else adopt
  the account with the same login, else JIT-create it (first unused
  directory email, else a noreply address) → apply the profile (name,
  verified emails, SSH/GPG keys flagged `ldap_synced`, site admin from the
  admin group — never demoting the last admin — and lifting a suspension
  made by sync) → apply LDAP team mappings for that user.
* Directory outcomes: unknown user → built-in accounts may still sign in;
  directory unreachable → built-in accounts still work, LDAP-linked ones
  are refused (site admins keep a break-glass built-in password).
* **Sync** (`ldap::sync`): service `accounts.ldap_sync` (every
  `sync_interval_hours`, Redis `SET NX` schedule + pg advisory lock),
  jobs `accounts.ldap_sync`, `accounts.ldap_sync_user`,
  `accounts.ldap_sync_team`. Linked users whose entry is gone, disabled
  (`userAccountControl` bit 2, `nsAccountLock`, `pwdAccountLockedTime`) or
  outside the restricted group are suspended (`suspended_reason` "LDAP: …",
  sessions destroyed, `user.suspend` audit with `ldap: true`,
  `UserAccountChanged` event); the sync lifts only suspensions it made
  (`ldap_user_sync.suspended_by_sync`), never manual ones. Profiles and
  admin status are refreshed; mapped teams get exactly the members of their
  groups (`member` / `uniqueMember` DNs and `memberUid`, no nested groups)
  that have active linked accounts; non-org members are added to the org.
* **Team sync** (`bgh_accounts::group_sync`, table
  `external_group_mappings(provider, external_group_id, team_id)`, reusable
  by P49): providers `ldap` (normalized group DN) and `oidc` (values of the
  groups claim).

### OIDC groups claim

`OidcProvider.groups_claim` (`BGH_OIDC_GROUPS_CLAIM`): when set and the
claim is in the ID token (or userinfo), the user's mapped teams are synced
at every sign-in (join mapped teams of their groups, leave the others).
Mappings: GitHub's team-sync REST, `GET|PATCH
/orgs/{org}/teams/{team_slug}/team-sync/group-mappings` (and the
`/teams/{id}` forms), `{groups: [{group_id, group_name,
group_description}]}`; PATCH needs an org owner (`admin:org`), GET owners
or maintainers.

### Endpoints

| Endpoint | Notes |
|---|---|
| `PATCH /admin/ldap/users/{username}/mapping {ldap_dn}` | 200 simple user + `name`, `email`, `ldap_dn`; `""` unmaps; 422 without `ldap_dn` |
| `POST /admin/ldap/users/{username}/sync` | 201 `{"status":"queued"}` (job) |
| `PATCH /admin/ldap/teams/{team_id}/mapping {ldap_dn}` | 200 team + `ldap_dn` |
| `POST /admin/ldap/teams/{team_id}/sync` | 201 `{"status":"queued"}` |
| `POST /_bgh/admin/ldap/test {settings?, login?}` | `{ok, message, user?}`: connect, service bind, optional lookup |
| `POST /_bgh/admin/ldap/sync` | full sync now → report `{users, suspended, teams, team_members_added, team_members_removed}` |
| `GET|PATCH .../team-sync/group-mappings` | see above |
| `GET /_bgh/site` | adds `ldap`, `password_login_admin_exempt` |

### Web

* Site admin → Settings → Authentication: password sign-in switch with the
  admin break-glass switch, an LDAP block (connection, bind, search,
  groups, attribute mapping / CA under a disclosure, JIT, sync interval,
  "Test connection" with an optional username lookup, "Sync now"), and a
  "Groups claim" field in the OIDC provider dialog. Form model in
  `settingsForm.ts` (`LdapForm`, `ldapValue`, `ldapErrors`, unit tests).
* Sign-in page reads `/_bgh/site`: with `password_login` off and no LDAP
  the password form is hidden (SSO only), with a "Site administrator
  sign-in" link when admins are exempt; with LDAP only the label is "LDAP
  username" and "Forgot password?" is hidden.

## Tables / migrations

`2600_directory_auth.sql`: `external_group_mappings`, `ldap_user_sync`,
`ssh_keys.ldap_synced`, `gpg_keys.ldap_synced`.

## Shared-code changes (additive)

* `bgh-core/src/auth.rs`: `check_password`, `PasswordTransport`,
  `PasswordDirectory` / `DirectoryAuth` / `set_password_directory`,
  `find_login`, `login_fail_key` / `ip_fail_key` and the throttle
  constants, `password_login_disabled`, `too_many_failed_logins`,
  `request_ip` (+ task-local peer in `auth_headers_middleware`);
  `basic_auth` uses `check_password`.
* `bgh-core/src/settings.rs`: `LdapSettings`, `AuthProviderSettings.{ldap,
  password_login_admin_exempt}`, `OidcProvider.groups_claim`, public info.
* `bgh-core/src/config.rs`: `BGH_OIDC_GROUPS_CLAIM`.
* Workspace deps: `ldap3` (rustls/ring), `ldap3_proto` (fake server,
  `bgh-accounts` feature `testing`), `rustls`, `rustls-pki-types`.

## Tests

LDAP tests run against an in-process fake LDAP server built on
`ldap3_proto` (`bgh_accounts::ldap::fake::FakeLdap`, feature `testing`;
binds, base/subtree searches, `&|!`, equality, presence, substring
filters) through the real `ldap3` client. No system slapd needed.

* `bgh-accounts/tests/it/ldap.rs`: web + `_bgh/auth` + git basic login with
  JIT provisioning (emails, SSH keys, identity link), profile/key refresh,
  linked accounts refuse built-in passwords, unknown users fall back,
  JIT off, directory down; admin group grants/revokes site admin,
  restricted group; sync suspends disabled and deleted users (sessions
  ended, audited, idempotent), lifts its own suspension but not manual
  ones; team mapping adds/removes members (and org membership) via GHES
  endpoints, jobs, full sync and sign-in; GHES user mapping + sync job;
  `password_login=false` on web/git (PATs keep working, LDAP unaffected,
  admin exemption, method validation); 20 bad git passwords → 10×401 then
  429, audit rows with `transport: "git"`, lockout shared with web.
* `sso_avatars_ratelimit.rs`: OIDC groups claim + team-sync REST.
* Web: `settingsForm.test.ts` (LDAP round trip, validation).
* UI verified with Playwright against a real `bgh serve` (built web):
  admin Settings → Authentication LDAP block (client validation, "Test
  connection" error against a closed port, save, write-only bind password
  after reload), password sign-in off + admin exemption; sign-in page shows
  "LDAP username" with LDAP, SSO-only mode hides the password form, and the
  "Site administrator sign-in" link reveals it for break-glass sign-in.
* Git password errors are plain text since the push-hardening merge (git
  shows them as `remote: …`); the test reads the text body.

## Known gaps / TODO

* Nested LDAP groups and AD `memberOf` are not expanded; group membership
  comes from the group entry.
* LDAP-sourced emails are added, never removed.
* The OIDC groups claim is read from the ID token, and from userinfo only
  when the ID token lacked an email (existing userinfo rule).
* No LDAP referral chasing or paged results (large directories with
  server-side size limits may truncate team sync).
