Integration: ready
P34 API compatibility headers, conditional requests, wrong-method 404s and root endpoints (`/markdown`, `/emojis`, `/zen`, `/octocat`, `/versions`, `/meta` SSH keys).

# P34 — API compatibility headers and root endpoints

Branch `bgh/p34-api-compat`. No migrations (range 4600–4699 unused).

## Implemented

Headers (bgh-server `api_compat`, root layer, every `/api/*` path):
* `X-GitHub-Enterprise-Version: <COMPAT_GHES_VERSION>`, including on
  Renovate's `HEAD /api/v3/`, on 404s and on `/api/graphql`.
* `X-GitHub-Request-Id`, the same value as `x-request-id` (a
  client-supplied id is kept).
* `X-GitHub-Api-Version` (REST only): a value outside
  `bgh_core::API_VERSIONS` (`2022-11-28`, `2026-03-10`) gets a 400
  `{"message":"API version X is not supported.", documentation_url,
  status}`. A supported value is echoed in `X-GitHub-Api-Version-Selected`
  (default `2022-11-28`). Both versions are served with the same shapes.
* `X-Accepted-OAuth-Scopes` on scope-gated endpoints. `AuthContext::
  require_scope` records the scope in a task-local that
  `auth_headers_middleware` installs. The header lists the scope plus every
  scope that implies it (`read:org` → `admin:org, read:org, write:org`).

Wrong method: the nested API router has a `method_not_allowed_fallback`
that returns a JSON 404. `api_compat` also turns 405s from API routes
mounted at the root (`/api/v3/`, `/api/graphql`) into JSON 404s.

Conditional requests (bgh-server `etag`):
* `Last-Modified` from a top-level JSON object's `updated_at`. A handler's
  own `Last-Modified` header is respected. Lists get none, because removing
  an item wouldn't move it.
* `If-Modified-Since` → 304 when there is no `If-None-Match`, which takes
  precedence (RFC 9110).
* Validators are computed only for JSON bodies with an exact size of 1 MiB
  or less, or when the request is conditional. Large or streamed bodies are
  no longer buffered just to hash them.
* Layer order: etag → api_headers → ratelimit (outermost). A 304 is
  refunded via `bgh_core::ratelimit::refund`, so `X-RateLimit-Remaining`
  doesn't drop.

Root endpoints (bgh-accounts `root.rs`):
* `POST /markdown` (`text`, `mode` `markdown`|`gfm`, `context`
  `owner/repo`) → `text/html;charset=utf-8`. In gfm mode, `@mentions` are
  linked, and `#123` and SHAs are linked against the context. A missing
  `text` or a bad `mode` → 422.
* `POST /markdown/raw` (body as markdown, plain mode).
* `GET /emojis`: 1913 gemoji names → `{base}/_bgh/emoji/{code}.svg`.
  `GET /_bgh/emoji/{code}.svg` serves bundled Twemoji SVGs (gzip passthrough
  when accepted, immutable caching, CSP `default-src 'none'`). Assets and
  attribution are in `crates/bgh-accounts/assets/emoji/` (NOTICE.md:
  Twemoji CC-BY 4.0, gemoji MIT). `scripts/build-emoji-assets.mjs`
  regenerates them.
* `GET /zen` (text/plain), `GET /octocat?s=` (application/octocat-stream,
  original ASCII art), `GET /versions`.
* API root: removed `authorizations_url`, `feeds_url`, `gists_url`,
  `public_gists_url` and `starred_gists_url` (re-add with P72/P80). A test
  asserts that every template-free root URL resolves.

`/meta` (bgh-graphql): `ssh_keys` (`ssh-ed25519 AAAA…`) and
`ssh_key_fingerprints.SHA256_ED25519` come from the host key via the new
`bgh_repos::ssh::host_public_keys`. They are empty when SSH is disabled or
the key hasn't been generated yet.

## Shared-code changes (additive)
* `bgh-core/src/lib.rs`: `API_VERSIONS`.
* `bgh-core/src/auth.rs`: `accepted_scopes_header`, the task-local scope
  recording in `require_scope`, and `auth_headers_middleware` emitting the
  header.
* `bgh-core/src/ratelimit.rs`: `refund`, and 304 refund in `limit`.
* `bgh-repos/src/ssh/mod.rs`: `host_public_keys`.
* `docs/ARCHITECTURE.md`: headers and layer order.

## Tests
* `bgh-server/tests/it/api_compat.rs`: enterprise version and request id
  (HEAD root, 404, graphql), API version validation, wrong method → JSON
  404 (nested and root), 304 not counted, Last-Modified and
  If-Modified-Since precedence, X-Accepted-OAuth-Scopes.
* `bgh-server/tests/it/server.rs`: POST /user is now a JSON 404.
* `bgh-accounts/tests/it/root.rs`: markdown (gfm, plain, sanitize,
  validation, raw), emojis and images (plain and gzip), zen, octocat,
  versions, API root URLs.
* `bgh-graphql/tests/it/schema.rs`: `/meta` SSH keys.
* `bgh-core` unit test: accepted scope expansion.
* `scripts/gh-compat.sh` 57/57 and `scripts/api-smoke.sh` 45/45 pass.

## Web
No web UI changes: the web client calls none of these endpoints, so
`web/src/mock/` needs no additions. Client-side emoji and markdown parity
belongs to P35.

## Known gaps
* `X-Accepted-OAuth-Scopes` covers only `require_scope` checks. Implicit
  `repo` checks in `perms::effective` don't record it.
* Raw, diff and contents (non-JSON) responses still have no validators.
* No GitHub custom emojis (`:octocat:`, `:shipit:`).
