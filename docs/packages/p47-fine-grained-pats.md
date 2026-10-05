Integration: in progress
Fine-grained PATs (`bgh_pat_`), org token policies and approvals, narrow classic scopes.

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
