# P15 — Container registry (OCI) and Packages REST and UI

Status: **in progress** (branch `bgh/p15-container-registry`).

## What exists

New crate `crates/bgh-packages`, mounted in bgh-server (`router()`,
`web_router()`, `register()`).

### OCI Distribution v2 (`/v2/`, at the host root)

| Endpoint | Notes |
|---|---|
| `GET /v2/` | 200 for authenticated callers, else 401 + `WWW-Authenticate: Bearer realm="{base}/v2/token",service="{host}"` |
| `GET /v2/token`, `POST /v2/token` | Docker token auth. Basic `user:<PAT or GITHUB_TOKEN>` (passwords → 401), Bearer PAT, or anonymous; `scope=repository:<name>:pull,push,delete` (repeatable, `*`). `POST` = OAuth2 `grant_type=password` (containerd). Returns `{token, access_token, expires_in: 300, issued_at}`: HS256 JWT with the granted `access` (key: `{data_dir}/packages/token.key`, created on first use) |
| `HEAD/GET /v2/{name}/blobs/{digest}` | single `Range` (206 / 416), `Docker-Content-Digest`, streamed |
| `DELETE /v2/{name}/blobs/{digest}` | unlinks from the package (202) |
| `POST /v2/{name}/blobs/uploads/` | session (202), monolithic `?digest=` (201), cross-repo `?mount=&from=` (201, falls back to 202 when not mountable or `from` unreadable) |
| `PATCH/PUT/GET/DELETE /v2/{name}/blobs/uploads/{uuid}` | chunked with `Content-Range` checks (416), final `PUT ?digest=` verifies, status, cancel |
| `HEAD/GET/PUT/DELETE /v2/{name}/manifests/{tag|digest}` | OCI manifest + index, Docker schema2 manifest + list; referenced blobs must be in the package (`MANIFEST_BLOB_UNKNOWN`; foreign/non-distributable layers exempt); digest of a by-digest push verified (sha256/sha512); `OCI-Subject`; DELETE by tag removes the tag, by digest deletes the version (restorable via REST) |
| `GET /v2/{name}/tags/list?n=&last=` | sorted, `Link: rel="next"` |
| `GET /v2/{name}/referrers/{digest}?artifactType=` | OCI 1.1 index of manifests with that `subject`, `OCI-Filters-Applied` |

Errors use the distribution-spec JSON (`{"errors":[{code,message,detail}]}`)
with `Docker-Distribution-API-Version: registry/2.0`. Names are
`{owner}/{package}[/...]` (lowercase). Missing access: 401 challenge for
anonymous callers and JWTs lacking the scope, 404 `NAME_UNKNOWN` for
authenticated callers who can't see the package, 403 `DENIED` otherwise.
A dedicated registry hostname works by pointing it at the same server
(`/v2/` is at the root); `docker login <BGH host>` uses `BGH_BASE_URL`'s host.
bgh-server's compression layer skips `application/vnd.oci.*` /
`application/vnd.docker.*` bodies (clients digest the exact bytes).

### Access (`bgh_packages::access`)

* Linked to a repository → inherits its real grants (read → pull, write →
  push, admin → delete/settings); the repository being public does not make
  a private package public.
* Unlinked → owning user, org admins and the publisher (`created_by`) are
  admins; org members read.
* `public` → anyone pulls; `internal` → any signed-in user.
* Creating a package: the owner, or any org member.
* PATs need `read:packages` (beyond public content), `write:packages`,
  `delete:packages`. Sessions are unrestricted.
* Actions `GITHUB_TOKEN` (job tokens, `actions:repo:{id}` scope): push/pull
  the packages linked to their repository (read-only tokens: pull), create
  new packages under the repository owner (linked to the repository and
  taking its visibility), everything else like an anonymous caller.
  P8's `packages:write` job-token category was not on the integration
  branch; when it lands, `access::role` is the place to honor it.
* Linking on push: job token's repository, else
  `org.opencontainers.image.source` (manifest annotation or config label)
  pointing at a same-owner repository on this instance the pusher can write.
  A package linked on its first push takes the repository's visibility.

### Storage and GC

* Blobs content-addressed in `{data_dir}/packages/blobs/{alg}/{hh}/{hex}`,
  one copy per digest (dedupe across packages); uploads in
  `{data_dir}/packages/uploads/{uuid}`. Manifests live in the database.
* `packages.size` (bytes) = distinct linked blobs + manifests; counted in
  the owner's storage quota (`settings::owner_storage_used_kb`, also used by
  git push quota checks). Uploads/mounts/manifests over the total quota →
  403 `DENIED`.
* `packages.gc` service (hourly, leader via pg advisory lock,
  `gc::run(state, grace, retention)`): abandoned uploads (1 day), soft
  deletes older than 30 days, blob links no version references (1 day
  grace), then unlinked blobs (DB + disk) under an exclusive advisory lock
  that blob commits hold shared.

### REST (GitHub shapes, under `/api/v3`)

`GET /user/packages`, `/users/{username}/packages`, `/orgs/{org}/packages`
(`package_type` required → 422, `visibility`, pagination);
`GET|DELETE /user|users/{u}|orgs/{org}/packages/{type}/{name}`,
`POST .../restore`; `GET .../versions` (`state=active|deleted`),
`GET|DELETE .../versions/{id}`, `POST .../versions/{id}/restore`.
Deleting the last version → 400 with GitHub's message. Deletes/restores are
soft (30 days) and audited (`package.delete|restore`,
`package_version.delete|restore`, `package.update`).

### Web endpoints

`GET /_bgh/packages/{owner}` (`?q=`), `GET /_bgh/repos/{o}/{r}/packages`,
`GET|PATCH /_bgh/packages/{owner}/{type}/{name}` (visibility, link/unlink
repository; package admins).

### Webhooks

`Event::PackagePublished` / `PackageUpdated` (new in `bgh_core::events`,
emitted for repository-linked packages) → `package` deliveries
`published` / `updated` (builder `bgh-notify/src/payloads/packages.rs`).

### Web UI

Packages tab on user/org profiles (`/:owner?tab=packages`),
`/orgs/:owner/packages`, `/users/:owner/packages`, package page
`/{orgs|users}/:owner/packages/container/package/:name` (pull command with
tag picker and copy, versions with tags/digest/platforms/size, per-version
delete, settings: visibility, repository link, delete package), repo
sidebar "Packages" section. Mock backend in `web/src/mock/extra/packages.ts`.

## Tables (migration `2700_packages.sql`)

`packages`, `package_blobs`, `package_blob_links`, `package_versions`,
`package_version_blobs`, `package_uploads`.

## Shared-code changes

* `bgh-core/events.rs`: `PackagePublished`, `PackageUpdated` (+ name /
  repo_id / actor_id arms).
* `bgh-core/settings.rs`: `owner_storage_used_kb`, `owner_quota_headroom`;
  `quota_headroom` totals now include package storage.
* `bgh-notify/payloads`: `packages.rs` + one match arm.
* `bgh-server`: mounts the crate; compression skips OCI/Docker media types.

## Verification

* `cargo test -p bgh-packages` (tests/it: conformance categories, tokens
  and access, REST shapes, webhooks and GC).
* `scripts/registry-conformance.sh`: official OCI conformance suite +
  real docker client.

## Known gaps

* Only the container ecosystem (npm/maven/... are P81+).
* `package` webhooks only for repository-linked packages; no org-level
  `registry_package` events.
* No per-package collaborator/team grants (GitHub's "Manage access").
* Download counts are not tracked.
