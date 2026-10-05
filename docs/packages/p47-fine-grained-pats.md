Integration: in progress
Fine-grained PATs (`bgh_pat_`), org token policies and approvals, narrow classic scopes; backend, web UI, tests, Playwright smoke.

# P47 — Fine-grained PATs, org token policies, narrow classic scopes — status

Branch `bgh/p47-fine-grained-pats`, migration `5900_fine_grained_pats.sql`
(range 5900–5999).

## API contract

### Web JSON (browser session only, like `/_bgh/tokens`)

| Endpoint | Notes |
|---|---|
| `GET /_bgh/fine-grained-tokens` | the session user's fine-grained tokens, newest first: `FineGrainedToken[]` |
| `POST /_bgh/fine-grained-tokens` | create → 201 `FineGrainedToken` with `token` (shown once) |
| `GET /_bgh/fine-grained-tokens/{id}` | one token |
| `DELETE /_bgh/fine-grained-tokens/{id}` | 204 |
| `GET /_bgh/fine-grained-tokens/owners` | resource owners the user may pick: `[{id, login, avatar_url, type: "User"\|"Organization", fine_grained_allowed, requires_approval, max_lifetime_days}]` (self first, then orgs the user belongs to) |
| `GET /_bgh/fine-grained-tokens/permissions` | catalog: `{repository: Perm[], organization: Perm[], account: Perm[]}`, `Perm = {name, label, description, access: ("read"\|"write")[]}`; `metadata` is always read |
| `GET /_bgh/orgs/{org}/pat-policy` | org admins: `PatPolicy` |
| `PATCH /_bgh/orgs/{org}/pat-policy` | org admins: partial `PatPolicy` → `PatPolicy` |

Create body:

```json
{
  "name": "deploy bot",
  "description": "",
  "resource_owner": "acme",
  "expires_in_days": 30,
  "repository_selection": "selected",
  "repository_ids": [12],
  "repositories": ["web"],
  "permissions": {
    "repository": {"contents": "read", "statuses": "write"},
    "organization": {"members": "read"},
    "account": {}
  },
  "reason": "why the org should approve"
}
```

`repository_selection`: `all` | `selected` | `public` (public repositories,
read-only). `expires_in_days` is mandatory (1–366, and at most the resource
owner's `max_lifetime_days`). `repositories` are names in the resource
owner's account (alternative to `repository_ids`).

`FineGrainedToken`:

```json
{
  "id": 7, "name": "deploy bot", "description": "",
  "token_last_eight": "abcd1234",
  "resource_owner": {"login": "acme", "id": 3, "avatar_url": "…", "type": "Organization", …SimpleUser},
  "repository_selection": "selected",
  "repositories": [{"id": 12, "name": "web", "full_name": "acme/web", "private": true}],
  "permissions": {"repository": {"contents": "read", "metadata": "read"}, "organization": {}, "account": {}},
  "status": "active" | "pending" | "denied" | "revoked",
  "expires_at": "…", "last_used_at": null, "created_at": "…",
  "token": "bgh_pat_…"   // creation response only
}
```

`PatPolicy`:

```json
{
  "fine_grained_allowed": true,
  "fine_grained_require_approval": false,
  "fine_grained_max_lifetime_days": null,
  "classic_allowed": true,
  "classic_max_lifetime_days": null
}
```

### GitHub REST (org admins; session or a token with `admin:org`)

| Endpoint | Notes |
|---|---|
| `GET /orgs/{org}/personal-access-token-requests` | pending requests (`organization-programmatic-access-grant-request`), `owner[]`, `sort=created_at`, `direction`, paginated |
| `POST /orgs/{org}/personal-access-token-requests` | `{pat_request_ids?, action: approve\|deny, reason?}` → 202 `{}` |
| `POST /orgs/{org}/personal-access-token-requests/{pat_request_id}` | `{action, reason?}` → 204 |
| `GET /orgs/{org}/personal-access-token-requests/{pat_request_id}/repositories` | minimal repositories |
| `GET /orgs/{org}/personal-access-tokens` | approved tokens (`organization-programmatic-access-grant`), paginated |
| `POST /orgs/{org}/personal-access-tokens` | `{action: revoke, pat_ids}` → 202 `{}` |
| `POST /orgs/{org}/personal-access-tokens/{pat_id}` | `{action: revoke}` → 204 |
| `GET /orgs/{org}/personal-access-tokens/{pat_id}/repositories` | minimal repositories |

Request/grant ids are the token id. Grant shape:
`{id, owner: SimpleUser, repository_selection: "none"|"all"|"subset",
repositories_url, permissions: {organization: {}, repository: {}, other: {}},
access_granted_at, token_id, token_name, token_expired, token_expires_at,
token_last_used_at}`; requests have `reason` and `created_at` instead of
`access_granted_at`.

## Enforcement (`bgh_core::pat`)

* **Fine-grained tokens** are `access_tokens` rows of kind `fine_grained`
  (`resource_owner_id`, `repository_selection`, `approval_status`,
  `permissions` JSONB, `access_token_repos`). Scopes mirror everything
  (`fgpat:owner:{id}`, `fgpat:selection:all|public`, `fgpat:repo:{id}`,
  `fgpat:pending`, repository categories as P8's
  `actions:permission:{cat}:{access}`, others as
  `fgpat:perm:{group}:{name}:{access}`), so auth needs no extra query.
  * `perms::effective` → `pat::effective_cap`: covered repositories (owned
    by the resource owner, selected/all, approved, not policy-blocked) get
    Write if any repository permission is `write`, else Read, **min'd with
    the user's own role**; others are anonymous-like (private → 404).
  * `token_permissions::middleware` → `pat::guard`: repository calls use
    the P8/P17 route-category table (`TokenPermissions::of` returns the
    map for fine-grained tokens too, so git transport and the GraphQL
    mutation guard apply unchanged); writes to repositories the token
    doesn't cover are refused; `/user/**` maps to account permissions
    (`GET /user`, `/user/repos`, `/user/orgs` pass), `/orgs/{org}/**`
    writes need the org to be the resource owner plus `members` /
    `administration` / `projects`; notifications, authorizations, admin,
    `POST /user/repos`, `POST /orgs/{org}/repos` and `/_bgh` writes are
    refused. 403 message: "Resource not accessible by personal access token".
  * `AuthContext::has_scope` maps classic scopes to fine-grained
    permissions (`repo` is never implied; `workflow` → `workflows: write`,
    `read:org` → org `members`/`administration` read, …).
  * `perms::readable_repos` (search, feeds, `/issues`): covered private
    repositories only (`perms::private_readable_ids`, new helper).
  * No `X-OAuth-Scopes` for fine-grained tokens; LFS calls
    `pat::check_git` (contents).
* **Org policy** (`org_pat_policies`): fine-grained allowed / require
  approval / max lifetime (≤ 366 d), classic allowed / max lifetime.
  `token_auth` computes blocking orgs in the same query
  (`pat::BLOCKED_ORGS_SQL`, excluding Actions job tokens) and adds
  `pat:blocked_org:{id}` scopes (filtered from `X-OAuth-Scopes`): blocked
  org repositories fall to the public floor, `/orgs/{org}/…` → 403.
  Creation enforces allowed + max lifetime (422). Approval: tokens of org
  members start `pending` unless the creator is an org admin or the token
  is public-only without org permissions (like GitHub); approve / deny /
  revoke rewrite `approval_status` and the `fgpat:pending` scope.
* **Narrow classic scopes**: a classic token without `repo` but with
  `repo:status` / `repo_deployment` gets, on private repositories, Write
  for the `statuses` / `deployments` categories and Read for metadata of
  the current REST request (`pat::with_request_need` task-local set by the
  middleware, read by `pat::narrow_cap`); git, GraphQL and other
  categories stay 404. `public_repo` keeps working as before.

Audit: `personal_access_token.create|destroy` (with `fine_grained`),
`personal_access_token.request_created|request_approved|request_denied|access_revoked`,
`org.personal_access_token_policy_update`.

## Web

* `/settings/tokens`: "Fine-grained tokens" list (status badges, owner,
  repositories, permission summary, delete) above "Tokens (classic)";
  create flow `/settings/tokens/new?type=fine-grained` (owner with policy
  hints, expiry within max lifetime, public/all/selected repositories via
  the generalized `RepoAccessPicker`, permissions per group, reason when
  approval is required, one-time token display).
  `pages/settings/developer/FineGrainedTokens.tsx`, logic in
  `fineGrained.ts` (+ tests).
* `/organizations/:org/settings/personal-access-tokens` (nav "Third-party
  Access", `g k`): policy form, pending requests (approve/deny with
  reason), active tokens (revoke, selected repositories).
  `pages/orgsettings/OrgPatPage.tsx`. Both are lazy chunks; initial bundle
  144.3 / 150 KB gzip after merging the integration branch.
* Mock: `src/mock/extra/fineGrainedTokens.ts` (+ test). API wrappers:
  `src/api/fineGrainedTokens.ts`.
* Playwright: `web/scripts/pat-smoke.mjs <url>` — real server: owner signs
  up, creates an org, requires approval; a second user joins, creates an
  org token (pending), owner approves, token active and reads
  `/orgs/{org}` over REST. Passed (27 checks) against a real server;
  `BGH_MOCK=1` against `dev:mock`.

## Tests

`cargo test -p bgh-accounts --test it fine_grained::` — selected repo +
`contents:read` (REST read, write 403, other repo 404, git clone ok /
push refused / other repo refused, account endpoints, `/user/repos`
filtering, deletion), write permissions vs user role and uncovered public
repos, validation (422 fields) + owners + catalog, approval flow (REST
shapes, pagination `Link`, owner filter, approve/deny/revoke, 404/422),
org policies blocking classic and fine-grained tokens (prohibition,
lifetimes), narrow classic scopes (`repo:status` posts a status to a
private repo and can't read contents or deploy; `repo_deployment`
deploys; git refused). Unit tests in `bgh_core::pat`.

## Shared-code changes (additive)

* `bgh-core`: new `pat` module; `crypto::{FINE_GRAINED_PAT_PREFIX,
  new_fine_grained_pat}`; `auth`: `has_scope` fine-grained branch,
  `scopes_header` hides internal scopes, `token_auth` selects `kind` +
  blocked orgs, `bgh_pat_` accepted in Basic auth; `perms`: two branches in
  `effective`, fine-grained branch in `readable_repos`, new
  `private_readable_ids` (the old inline query, plus owner/id filters);
  `token_permissions`: `of` covers fine-grained tokens, middleware
  delegates to `pat::guard` when `pat::applies`.
* `bgh-repos`: `pat::check_git` in LFS access.
* Web: `RepoAccessPicker` generalized (optional public choice); org
  settings nav entry; routes.

## Notes for concurrent packages

* P36 (sudo before creating PATs): `bgh_accounts::fine_grained::create`
  should get the same `bgh_core::sudo::require` call as
  `tokens::create_token` once P36 lands (not referenced here since the
  module isn't on the integration branch yet). P36's
  `GitHub-Authentication-Token-Expiration` header applies to fine-grained
  tokens automatically (same `token_auth`).
* P46/P48: `pat::guard` runs before the integration-token logic in
  `token_permissions::middleware`; it only applies to personal access
  tokens (`pat::applies`).

## Known gaps / TODO

* GraphQL mutations by fine-grained tokens are checked per category but
  not against the token's repository list (e.g. `createIssue` on an
  uncovered public repository passes; REST refuses it).
* Organization permissions are org-agnostic in `has_scope` (reads of
  other orgs' member lists via `read:org` checks are possible when the
  owner org grants `members: read`); writes are restricted to the owner.
* No notification/email to the requester on approve/deny; the deny
  reason isn't shown to the user (`review_reason` is stored).
* Editing a token (repositories/permissions) and regenerating it are not
  implemented; `GET /orgs/{org}/personal-access-token-requests` ignores
  `repository`, `permission`, `last_used_*` filters.
* Internal repositories reach fine-grained tokens only through explicit
  grants (not through internal visibility).
