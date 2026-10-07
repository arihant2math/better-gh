Integration: landed
SAML 2.0 SSO (pure-Rust SP: metadata, SP/IdP-initiated sign-in, signed + encrypted assertions, SLO, JIT, attribute mapping, group sync) and SCIM 2.0 (enterprise Users/Groups, org Users, deprovisioning revokes credentials).

# P49 saml-scim — status

Branch `bgh/p49-saml-scim`. Scope: `docs/PHASE4_PLAN.md` §P49 (no §5 quick
fixes are assigned to P49). Migrations: 6100–6199 (`6100_saml_scim.sql`).
Builds on P14 (LDAP): SAML is another directory provider next to it and
reuses its login enforcement (`password_login`,
`password_login_admin_exempt`; SAML counts as a sign-in method) and its
`external_group_mappings` team sync.

## SAML 2.0 (`bgh_accounts::saml`)

No `samael`/libxmlsec: a small pure-Rust stack, so the Dockerfile needs no
new system packages.

* `saml::xml` — namespace-aware tree on the `xmlparser` tokenizer (exact
  prefixes kept, DTDs/entities refused, depth/size limits) and
  canonicalization: C14N 1.0 (also used for 1.1) and exclusive C14N with
  `InclusiveNamespaces`, comments dropped (same-document references).
* `saml::dsig` — enveloped XML-DSig verify/sign: RSA-SHA1/256/384/512,
  SHA-1/256/384/512 digests. Anti-wrapping rules: the signature is a direct
  child of the signed element, exactly one `Reference` = `#` + that
  element's `ID`, the ID is unique in the document, only enveloped + C14N
  transforms. Keys come from the configured IdP certificates only (several
  allowed for rollover), never from `KeyInfo`.
* `saml::xmlenc` — `EncryptedAssertion`: RSA-OAEP (MGF1 SHA-1/SHA-256)
  key transport, AES-128/256-CBC and AES-128/256-GCM; RSA PKCS#1 v1.5 key
  transport refused; uniform errors.
* `saml::certs` — X.509 parsing (`x509-cert`), SP key pair generation
  (RSA 2048, self-signed 10 years).
* `saml::response` — Response validation: version, `Destination`,
  status, issuer (response and assertion), `InResponseTo` (response and
  subject confirmation) against the pending request, exactly one
  (encrypted) assertion as a direct child, signature on the response or
  the assertion (required), bearer `SubjectConfirmationData`
  (`Recipient`, `NotOnOrAfter`, `NotBefore`), `Conditions` window and
  `AudienceRestriction` (required, must contain the SP entity ID), clock
  skew setting; attributes by `Name` and `FriendlyName`.
* `saml::provision` — account resolution: `user_identities` (provider
  `saml`, subject = NameID) → enterprise SCIM user with that `userName` →
  account with the same login (not linked to another SAML subject) → JIT
  (login from the username attribute or NameID, first unused email else a
  noreply address). Attributes (GHES defaults): `full_name`, `emails`
  (verified), `public_keys` / `gpg_keys` (`saml_synced` rows, replaced
  when the attribute is present; shared helper `directory_keys` with
  LDAP), `admin_attribute` (off by default; never demotes the last site
  admin), `groups` → `group_sync::apply_user_groups(provider "saml")`.
  Suspended accounts are refused; 2FA still applies.

Endpoints:

| Endpoint | Notes |
|---|---|
| `GET /saml/metadata` | SP metadata (entity ID, ACS POST, SLO Redirect, NameIDFormat, KeyDescriptors when an SP cert is set); served from the stored settings even while disabled |
| `GET /_bgh/saml/login?return_to=` | HTTP-Redirect `AuthnRequest` (signed with the SP key when `sign_requests`); pending request (ID, return_to) in Redis under a random RelayState (10 min) |
| `POST /saml/consume` | ACS (form `SAMLResponse`, `RelayState`): validates, replay cache (assertion ID in Redis until expiry), session cookie, 303 to `return_to`; failures → `/login?error=` (details only in logs). Unsolicited responses only with `allow_idp_initiated` (RelayState used as a same-site path). CSRF-exempt |
| `GET /saml/sls` | IdP `LogoutRequest` (HTTP-Redirect, signature required) ends every session of the linked user, answers a `LogoutResponse` to `idp_slo_url`; a `LogoutResponse` → `/login` |
| `POST /_bgh/saml/logout` | SP-initiated logout: ends the session, `{redirect}` = IdP logout URL with a `LogoutRequest` (SAML-linked users, `idp_slo_url` set) or `/login` |
| `GET /_bgh/admin/saml` | site admins: entity ID, ACS/SLO/metadata/login URLs, parsed IdP/SP certificates (subject, SHA-256 fingerprint, expiry), errors |
| `POST /_bgh/admin/saml/keypair` | 201 `{certificate, private_key, fingerprint_sha256}` (not stored) |
| `POST /_bgh/admin/saml/idp_metadata` | `{metadata}` XML or `{url}` → `{idp_entity_id, idp_sso_url, idp_slo_url, idp_certificate}` |
| `GET /_bgh/site` | adds `saml: null | {display_name, login_url}` |

Settings `auth_providers.saml` (`bgh_core::settings::SamlSettings`):
enabled, display_name, idp_sso_url, idp_entity_id, idp_certificate,
idp_slo_url, sp_entity_id (default base URL), sp_certificate,
sp_private_key (write-only, redacted like other secrets), sign_requests,
require_encrypted_assertions (needs the SP key), name_id_format,
allow_idp_initiated, jit_provisioning, attribute names, clock_skew_seconds.
Validated by the admin settings API. Private mode exempts `/saml/*` and
`/_bgh/saml/login`.

## SCIM 2.0 (`bgh_accounts::scim`)

Enabled by `auth_providers.scim.enabled` (404 otherwise). Under `/api/v3`:

| Endpoint | Auth |
|---|---|
| `GET|POST /scim/v2/enterprises/{enterprise}/Users`, `GET|PUT|PATCH|DELETE …/Users/{id}` | site admin + token scope `scim:enterprise` (new, site-admin-only scope; sessions pass) |
| `GET|POST /scim/v2/enterprises/{enterprise}/Groups`, `GET|PUT|PATCH|DELETE …/Groups/{id}` | same |
| `GET|POST /scim/v2/organizations/{org}/Users`, `GET|PUT|PATCH|DELETE …/Users/{id}` | org owner (or site admin) + `admin:org`; non-members 404 |

* GitHub/RFC shapes: `schemas`, `id` (UUID), `externalId`, `userName`,
  `displayName`, `name`, `emails`, `roles` (enterprise), `active`, `meta`
  (`location`); ListResponse with `totalResults`/`startIndex`/
  `itemsPerPage`; errors `{schemas: [Error], status, scimType, detail,
  message, documentation_url}`; `application/scim+json`; 201 + `Location`.
* Filters: `attr eq "value"` joined by `and` (Users: userName
  (case-insensitive), externalId, id, displayName, emails[.value], active;
  Groups: displayName, externalId, id); `startIndex`/`count` (≤ 1000).
* PATCH: add/replace/remove with or without path, Azure AD capitalized ops
  and string booleans, `emails[type eq "work"].value`,
  `members[value eq "id"]`.
* Enterprise users: create links the account with the same login (or
  local part of an email userName) or verified email, else creates one;
  `active: false` / `DELETE` → `scim::deprovision`: suspend (reason
  `SCIM: …`, site admin dropped, last admin never), revoke sessions, PATs,
  OAuth tokens and SSH keys immediately, audit `user.suspend {scim,
  revoked_tokens, revoked_ssh_keys}`; `active: true` lifts only a SCIM
  suspension (`scim_users.suspended_by_scim`). `roles` with
  `enterprise_owner` → site admin.
* Groups → teams: `external_group_mappings` provider `scim`, matched by
  display name (case-insensitive), SCIM id or externalId; a matched team's
  members = active, unsuspended accounts of its mapped groups
  (`group_sync::set_team_members`, adds org membership as needed); synced
  on group create/PUT/PATCH/DELETE, user (de)activation/delete, and when
  mappings change. Teams with no matching SCIM group are never touched.
* GitHub's team-sync REST (`PATCH …/team-sync/group-mappings`) now maps
  the team for all IdP providers at once (`oidc`, `saml`, `scim`).
* Organization users: create adds the matching account to the org
  (creates the account only when sign-up is open or the caller is a site
  admin, else 403); `active: false` / `DELETE` remove the membership only.

## Web

* Sign-in page: "Sign in with {display_name}" SSO button when
  `/_bgh/site` has `saml` (also in SSO-only mode).
* Site admin → Settings → Authentication: SAML block (IdP fields, IdP
  metadata import from XML or URL, SP panel from `GET /_bgh/admin/saml`
  with copyable URLs and certificate fingerprints/expiry/errors, SP
  certificate + write-only key with "Generate key pair", switches,
  NameID format, attribute mapping, clock skew); form model and
  validation in `settingsForm.ts` (`SamlForm`, `samlValue`, `samlErrors`,
  unit tests). "SCIM provisioning" switch with endpoint URLs.
* New lazy page `/site-admin/scim` (nav "SCIM provisioning", `g v`):
  status, endpoint URL, "Generate SCIM token" (`scim:enterprise`, shown
  once), paginated provisioned users (userName filter) and groups.
* `scopes.ts`: `scim:enterprise` (site admins only). Mocks in
  `web/src/mock/extra/saml.ts` (+ tests).
* Bundle: initial JS unchanged (144.6 KB gzip after the merge); settings
  chunk +3.4 KB, new SCIM chunk 3.5 KB.
* Verified with Playwright against `npm run dev:mock` (light/dark) and
  against a real `bgh serve`: enable SAML+SCIM, settings SAML block,
  generate key pair + save (key stored write-only, SP cert shown), SCIM
  page, sign-in button, and a browser IdP-initiated sign-in with a
  response signed by signxml (lands signed in as the JIT user).

## Tables / migrations

`6100_saml_scim.sql`: `ssh_keys.saml_synced`, `gpg_keys.saml_synced`,
`scim_users` (tenant `org_id` NULL = enterprise; unique userName and user
per tenant), `scim_groups`, `scim_group_members`.

## Shared-code changes (additive)

* `bgh-core/src/settings.rs`: `SamlSettings`, `ScimSettings`,
  `AuthProviderSettings.{saml, scim}`, `public_info.saml`.
* `bgh-core/src/auth.rs`: `scim:enterprise` in the scope list, CSRF
  exemption for `/saml/consume`.
* `bgh-core/src/privacy.rs`: private-mode exemptions `/saml/`,
  `/_bgh/saml/login`.
* `bgh-admin/src/settings.rs`: SAML validation, `sp_private_key`
  redaction, SAML as a sign-in method.
* `bgh-accounts`: `directory_keys` (LDAP key sync moved here and shared),
  `sso::{safe_return_to, sso_error, available_login}` now `pub(crate)`,
  `group_sync::IDP_PROVIDERS`; `tokens::KNOWN_SCOPES` +
  `scim:enterprise` (site admins only, also in `bgh admin create-token`).
* Workspace deps: `xmlparser`, `x509-cert` (builder, pem), `aes`, `cbc`,
  `aes-gcm`.

## Tests

* Unit (`saml::tests`, `saml::xml`, `scim`): fixtures in
  `crates/bgh-accounts/src/saml/testdata/` signed by signxml/lxml and
  encrypted with Python `cryptography` (`gen.py`, independent of this
  implementation): exclusive-C14N assertion signature, inclusive-C14N
  response signature, an inclusive-C14N assertion signed out of context
  (must fail), AES-CBC + RSA-OAEP encrypted assertion; tampering,
  signature wrapping, duplicate IDs, unsigned, audience / destination /
  time / InResponseTo / required encryption; own sign/verify and
  encrypt/decrypt round trips; IdP metadata parsing; key pair generation;
  SCIM filters, PatchOp parsing and user field patches.
* `tests/it/saml.rs` (in-process test IdP `saml::testing`, feature
  `testing`): SP-initiated round trip (metadata, AuthnRequest, JIT,
  name/emails/SSH keys/admin attribute, replay refused, attribute changes
  on next sign-in), invalid responses (unsigned, expired, audience,
  issuer, recipient, other request, tampered, unsolicited, garbage) and a
  signed-response variant, required encryption + IdP-initiated + write-only
  SP key, signed IdP SLO and SP-initiated logout, groups attribute → team
  sync, admin endpoints and settings validation (incl. password_login off
  with SAML only).
* `tests/it/scim.rs`: enterprise Users conformance (auth/scope/disabled,
  create shape + Location + content type, uniqueness 409, invalid 400,
  get, filter by `userName eq` (case-insensitive) and `externalId`,
  pagination, invalidFilter, PATCH active=false suspends and revokes PATs,
  SSH keys and sessions, reactivation, PUT with enterprise_owner role,
  DELETE), Groups → team sync (create, add, remove by filter path,
  deactivation, rename, delete), organization Users (membership only,
  owner/scope rules, sign-up policy), SAML sign-in of SCIM users and
  refusal after deprovisioning.

## Known gaps / TODO

* One SAML IdP per instance (GHES model); no per-organization SAML.
* The web client's sign-out does not call `POST /_bgh/saml/logout` yet
  (SP-initiated SLO is available to API clients).
* No `EncryptedID` NameIDs, no Artifact binding, no signed
  `LogoutResponse` verification (responses only redirect to `/login`).
* Organization SCIM does not revoke credentials (it manages membership
  only; GitHub.com revokes SSO-authorized credentials per org, which this
  instance has no notion of).
* SCIM filters support `eq` (and `and`) only.
