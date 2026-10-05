Integration: pending (full gate running)
P33 repo metadata: licenses/gitignore endpoints, license detection, create templates + team_id, /repositories, branches-where-head, short SHAs; web pickers.

# P33 repo-metadata — status

Branch `bgh/p33-repo-metadata`. Scope: `docs/PHASE4_PLAN.md` §P33 (no §5
quick fixes assigned). Migration range 4500–4599 (used:
`4500_license_detection.sql`).

## Endpoints

| Endpoint | Notes |
|---|---|
| `GET /licenses[?featured=true]` | 13 commonly used (choosealicense `hidden: false`), `license-simple`, paginated (`Link` with `last`) |
| `GET /licenses/{key}` | full `license` object for all 47 vendored licenses, case-insensitive key |
| `GET /gitignore/templates` | 149 root templates of github/gitignore, sorted (byte order) |
| `GET /gitignore/templates/{name}` | `{name, source}`; `application/vnd.github.raw` → text |
| `GET /repos/{o}/{r}/license[?ref=]` | `license-content` (content-file + `license`), detected live for the ref; raw media type; 404 without a root license file |
| `POST /user/repos`, `/orgs/{org}/repos` | `gitignore_template`, `license_template` (each implies an initial commit; README only with `auto_init`; `[year]`/`[fullname]` filled with owner name or login), `team_id` (org team, granted the team's default permission; `TeamRepoAdded` emitted after `RepositoryCreated`). Unknown template / foreign team → 422 `Repository` field error |
| `GET /repositories/{id}` | same body as `/repos/{o}/{r}`; 404 for unknown/unreadable |
| `GET /repositories?since=` | public (+ readable internal) repos by id, per_page default/max 100, `since` Link pagination |
| `GET /repos/{o}/{r}/commits/{sha}/branches-where-head` | `branch-short` list; 422 "No commit found for SHA" |
| `git/blobs|commits|tags/{sha}` | unique abbreviated SHAs (7–39 hex); only full-SHA responses are cached as immutable |

GraphQL: `Repository.licenseInfo` now uses the vendored data (names,
nicknames, `other` for NOASSERTION); new root `licenses` and `license(key:)`.
Search `license:<key>` works once detection has run (keys = lowercase SPDX ids).

## License detection

`bgh_repos::licenses`: askalono store built once from the vendored
choosealicense texts (threshold 0.85). Root files ranked like licensee
(`LICENSE`, `LICENSE.md`, `COPYING`, `LICENSE-MIT`, …). Job
`repos.detect_license` (deduped like languages) runs when the default branch
moves (post-receive, incl. mirrors/imports and API writes), on default-branch
change, on fork and after creation with a license template. It writes
`license_spdx_id` (`NOASSERTION` = unrecognised file → GitHub's
`key: "other"`; NULL = no file) and `license_blob_sha` (skips re-analysis
when the blob is unchanged). Service `repos.license_backfill` queues
detection once after startup for repos never scanned (`license_blob_sha IS
NULL AND pushed_at IS NOT NULL`, partial index).

## Vendored data (no runtime fetch)

* `crates/bgh-core/data/licenses/*.txt` — choosealicense.com `_licenses` (MIT, `LICENSE.md`).
* `crates/bgh-repos/data/gitignore/*.gitignore` — github/gitignore root templates (CC0, `LICENSE`); list in `data/gitignore-names.txt`.
* `scripts/vendor-templates.sh` re-vendors both from raw.githubusercontent.com.

## Shared-code changes (additive)

* `bgh_core::licenses` (new module): parse/lookup, `simple()` → `api::LicenseSimple`, `render()`, `node_id()` (`07:License{key}`).
* `api::MinimalRepository::new`: `license` from `license_spdx_id` (was always null).
* Workspace dep `askalono = 0.5` (default features off; pulls zstd, rayon, rmp-serde).
* `bgh-graphql`: `misc::License::from_spdx` rewritten on `bgh_core::licenses`; two root fields in `query.rs`.

## Web

* `NewRepoPage`: .gitignore and license pickers enabled, loaded from the
  server (static fallback while loading), sent as `gitignore_template` /
  `license_template`; team picker for org admins (`team_id`).
* About sidebar already showed `license` from the REST repo JSON.
* Mocks: `src/mock/extra/licenses.ts` (+ test), mock repo create honours
  templates and `team_id`, mock repo JSON derives `license`.
* Verified with Playwright on `dev:mock` (pickers populated, Go + MIT sent, repo shows license).

## Tests

* `crates/bgh-repos/tests/it/metadata.rs` (8 tests: shapes, pagination,
  errors, templates + team, detection/other/ref/removal, backfill, GraphQL,
  search, `/repositories`, branches-where-head, short SHAs).
* Unit: license parsing, file ranking, detection of 6 licenses, gitignore lookup.
* `scripts/gh-compat.sh`: new cases `gh repo create --gitignore Go --license mit --team t`,
  license detected, `gh repo license list/view`, `gh repo gitignore list/view` (63/63 pass).

## Known gaps

* Detection covers the 47 choosealicense licenses only (like GitHub's licensee); no SPDX-expression / multi-license reporting.
* `/repositories/{id}/...` sub-resource aliases are not routed (only the repository itself).
* Legacy `tags/protection` and custom properties (AUDIT) stay out of scope.
