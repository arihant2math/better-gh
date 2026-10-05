Integration: ready
P22 done: check/status Link headers, commit node_ids, body media types, HTML-host .diff/.patch; full gate green after merging fc37a0d.

# P22 — Pulls and checks REST correctness

**Status:** done; awaiting the integrator (rule 14).
Branch `bgh/p22-pulls-rest`. Migrations: none (range unused).

## Implemented

* **Link headers + `total_count`** (`bgh-pulls/src/checks.rs` `Linked<T>`):
  `GET /repos/{o}/{r}/commits/{ref}/check-runs`,
  `/check-suites/{id}/check-runs`, `/commits/{ref}/check-suites` and
  `/commits/{ref}/status` now send RFC 5988 `Link` (next/last/prev/first)
  alongside the wrapped body. Combined status defaults `per_page` to 100
  (GitHub), `total_count` = number of contexts. `/commits/{ref}/statuses`
  already paginated with `Link`.
* **Commit `node_id`** = `Commit "{repo_id}:{sha}"` in `/pulls/{n}/commits`
  (`bgh-pulls/src/commits.rs`, `render`/`render_many` take `repo_id`) and
  `/search/commits` (`bgh-search/src/commits.rs`), matching repos API,
  webhooks and GraphQL `node()`.
* **Body media types** (`bgh-pulls/src/body.rs`): `Accept:
  application/vnd.github[.v3].{raw,text,html,full}+json` on PR, review and
  review-comment routes (get/list/create/update/submit/dismiss/reply/edit,
  `/commits/{sha}/pulls`) returns `body` / `body_text` / `body_html` like
  bgh-issues, and sets `X-GitHub-Media-Type: github.v3; param=<p>;
  format=json`. Implemented as a route wrapper (`body::formatted`) so the
  handlers GraphQL calls directly are unchanged; raw requests pass through
  untouched (no extra work).
* **HTML-host diff URLs** (`bgh-server/src/web.rs`, fallback in `lib.rs`):
  `/{o}/{r}/pull/{n}.diff|.patch`, `/{o}/{r}/commit/{sha}.diff|.patch`,
  `/{o}/{r}/compare/{a}...{b}.diff|.patch` are rewritten to the matching
  API request with the diff/patch media type (so read access, 404 on
  private repos, diff size limits all come from the API handler) and served
  as `text/plain; charset=utf-8`. Other paths still fall through to the SPA.

## Tests

* `bgh-pulls/tests/it/rest_compat.rs`: 45 check runs followed via `Link`
  (ref + suite lists), suites list, combined status (105 contexts, default
  100/page) and statuses list followed via `Link`; raw/full/html/text
  shapes for PR get/list, review create/list, review comment
  create/get/list, errors untouched; `.diff`/`.patch` for pull, commit,
  compare, private repo 404 for anonymous/outsider, 200 for owner.
* `bgh-search/tests/it/commit_node_ids.rs`: node_id equal across
  `/pulls/1/commits`, `/commits/{sha}`, `/search/commits`, and GraphQL
  `node(id:)` resolves it to the same Commit.

## Shared-code changes

None in `bgh-core`. `bgh-server` fallback gained the diff rewrite.

## Known gaps

* GraphQL `PullRequestCommit.id` still uses its own `pr:{pull}:{sha}` key
  (a different GitHub type; owned by the GraphQL packages).
* No octokit in the test toolchain; the Link-follow test stands in for
  `octokit.paginate`.
