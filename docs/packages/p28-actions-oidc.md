Integration: ready
P28 Actions OIDC id-tokens: issuer + discovery + JWKS, key rotation, job token requests, GitHub claims, sub customization (repo + org).

# P28 — Actions OIDC id-token

Branch `bgh/p28-actions-oidc`. Crate `bgh-actions` (new `oidc.rs`).
Migration `4000_actions_oidc.sql`. User docs: `docs/ACTIONS_OIDC.md`
(AWS, GCP and Azure trust setup), linked from `SELF_HOSTING.md`.

## Implemented

* **Issuer** `{base_url}/_services/token` (GHES convention):
  `GET /_services/token/.well-known/openid-configuration` (GitHub's
  discovery shape: `issuer`, `jwks_uri`, `subject_types_supported`,
  `response_types_supported`, `claims_supported`,
  `id_token_signing_alg_values_supported: [RS256]`, `scopes_supported`)
  and `GET /_services/token/.well-known/jwks` (`kty/alg/use/kid/n/e`,
  newest key first). Both `Cache-Control: public, max-age=300`.
* **Signing keys** (RS256, 2048-bit) in `{data_dir}/actions/oidc/` as
  PKCS#1 PEM `{created_unix:012}-{kid}.pem` (0600, `create_new`). First
  key generated lazily (blocking pool, process-wide lock); parsed keys
  cached per data dir and reloaded when the directory listing changes
  (rotation by another node sharing the dir). `oidc::rotate(state, force)`:
  the maintenance loop calls it every tick and rotates after 90 days;
  `POST /_bgh/admin/actions/oidc/rotate-key` (site admin, 201
  `{kid, keys}`, audited `actions.oidc_key_rotate`) forces it. The previous
  key stays in the JWKS; older keys are deleted once their successor is an
  hour old.
* **Job variables.** When the job's `GITHUB_TOKEN` permission map has
  `id_token: write` (so never for fork PRs, which are read-only, and never
  by default), `prepare_spec` sets `JobSpec.id_token_request_url`
  (new optional field, serde default) = `{issuer}/idtoken?api-version=2.0`;
  the runner then exports `ACTIONS_ID_TOKEN_REQUEST_URL` and
  `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (= the job's `GITHUB_TOKEN`, already
  masked). Without the permission neither is set.
* **Token endpoint** `GET|POST /_services/token/idtoken?audience=…`
  (`@actions/core` `getIDToken` compatible): `Authorization: Bearer|token
  <GITHUB_TOKEN>` → `{"value": "<jwt>"}`. 401 for unknown/expired tokens
  or jobs no longer `in_progress`; 403 when the token lacks
  `id-token: write`. Lifetime 5 min (`nbf` = iat − 5 s). Default `aud`:
  `{base_url}/{owner}` (GitHub's default).
* **Claims** (GitHub names, string values): `jti sub aud ref sha
  repository repository_owner repository_owner_id run_id run_number
  run_attempt repository_visibility repository_id actor_id actor workflow
  head_ref base_ref event_name ref_protected ref_type workflow_ref
  workflow_sha job_workflow_ref job_workflow_sha runner_environment
  (self-hosted) environment environment_node_id check_run_id iss nbf exp
  iat`. Jobs of reusable workflows get the called workflow as
  `job_workflow_ref` (engine now sets `github.job_workflow_ref` next to
  `job_workflow_sha` for called jobs).
* **`sub`**: template keys joined by `:`; `repo` → `repo:o/r`, `context`
  → `environment:<env>` | `pull_request` | `ref:<ref>`, others
  `key:value`. Default template `[repo, context]`.
* **Customization REST** (table `actions_oidc_sub_claims`):
  * `GET /repos/{o}/{r}/actions/oidc/customization/sub` (read access) →
    `{"use_default": true}` or `{"use_default": false,
    "include_claim_keys": [...]}`; `PUT` (repo admin; `use_default`
    required, 422 otherwise; unknown/forbidden claim keys 422) → 201 `{}`.
    `use_default: false` without keys opts into the org template.
  * `GET /orgs/{org}/actions/oidc/customization/sub` (members; 404 for
    non-members) → `{"include_claim_keys": [...]}` (default `["repo",
    "context"]`); `PUT` (org owners, 403 members; `include_claim_keys`
    required) → 201 `{}`. Writes audited
    (`repo.actions_oidc_sub_update`, `org.actions_oidc_sub_update`).

## Tables / migrations

`migrations/4000_actions_oidc.sql`: `actions_oidc_sub_claims` (repo or
org row, partial unique indexes) and `actions_jobs_token_idx` (token →
running job lookup).

## Shared-code changes

None in `bgh-core`. Signing-key storage lives in `bgh-actions::oidc`
(the plan suggested bgh-core; nothing else needs it yet — move it when
artifact attestations land). `crypto::write_private` became `pub(crate)`.
`JobSpec` gained `id_token_request_url` (additive, serde default).
New dev-dependency `jsonwebtoken` (tests verify tokens with it); `rsa`
added to bgh-actions from the workspace.

## Tests

`crates/bgh-actions/tests/it/oidc.rs`: discovery/JWKS shapes; a job with
`id-token: write` fetches a token that validates (jsonwebtoken, against
the discovery doc + JWKS) with the expected claims and default audience,
401 after completion and for bad tokens; no variables / 403 without the
permission; environment claim + sub; repo and org `sub` customization
(shapes, 422s, permissions, applied to tokens); key rotation keeps the old
key valid; an end-to-end shell-executor job curls the endpoint and the
token verifies, while a job without the permission sees no variables.
Unit tests for `render_sub` and template validation in `oidc.rs`.

## Gate

Merged `origin/claude/sleepy-cray-9jj0t3` (7ac1b95). fmt, clippy, web
typecheck/lint/test/build (initial JS 144.4 KB gzip, unchanged) green.
`cargo test --workspace --no-fail-fast`: all green except one
`bgh-uploads` `camo::proxies_external_images` failure (site-settings
cache timing, unrelated to this diff); it passes on re-run, as does the
whole `bgh-uploads` + `bgh-actions` suite.

## Known gaps

* No web UI for the `sub` template (REST/`gh api` only; the plan asked for
  none).
* `ref_protected` is always `false` (the `github` context does not compute
  it yet).
* No `enterprise`/`enterprise_id` claims (no enterprise object) and no
  `use_immutable_subject` template option.
* The request token is the job's `GITHUB_TOKEN` rather than a separate
  runtime token.
* Automatic rotation is per process; nodes sharing the data dir may each
  add a key on the same tick (harmless: all keys are published).
