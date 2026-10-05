Integration: landed
P31 Insights: /stats/*, traffic, community profile, activity log and Insights UI; full gate green after merging the integration branch.

# P31 — Repository Insights: stats, traffic, community profile, Insights UI — status

**Done.** Branch `bgh/p31-insights`. Migration `4300_insights.sql` (range
4300–4399). All code in `bgh-repos` and `web/`; no shared-crate changes.

## Endpoints (REST, relative to `/api/v3`)

| Method + path | Who | What |
|---|---|---|
| `GET /repos/{o}/{r}/stats/contributors` | readers | top 100 authors with an account (emails mapped like `/contributors`), ascending by `total`; `weeks` dense `{w,a,d,c}` over the whole history |
| `GET /repos/{o}/{r}/stats/commit_activity` | readers | last 52 weeks `{days[7] (Sunday first), total, week}` |
| `GET /repos/{o}/{r}/stats/code_frequency` | readers | `[week, additions, -deletions]` for every week; **422** for 10k+ commits |
| `GET /repos/{o}/{r}/stats/participation` | readers | `{all[52], owner[52]}` (owner = commits mapped to the owner account) |
| `GET /repos/{o}/{r}/stats/punch_card` | readers | 168 `[day, hour, commits]` (author local time) |
| `GET /repos/{o}/{r}/traffic/views` / `clones` | push access (403 `Must have push access to repository.`; 404 anonymous) | `{count, uniques, views|clones: [{timestamp, count, uniques}]}`, 14 dense days; `per=week` → Monday-based weeks; other `per` → 422 |
| `GET /repos/{o}/{r}/traffic/popular/paths` | push | top 10 `{path, title, count, uniques}` over 14 days |
| `GET /repos/{o}/{r}/traffic/popular/referrers` | push | top 10 `{referrer, count, uniques}` (referrer host, `www.` stripped) |
| `GET /repos/{o}/{r}/community/profile` | readers | `{health_percentage, description, documentation (homepage), files{code_of_conduct, code_of_conduct_file, contributing, issue_template, pull_request_template, license, readme, security*}, updated_at, content_reports_enabled}` |
| `GET /repos/{o}/{r}/activity` | readers | `[{id, node_id, before, after, ref, timestamp, activity_type, actor}]`, newest first; `direction`, `ref` (short or full), `actor`, `time_period` (day/week/month/quarter/year), `activity_type`; page pagination + `Link` |
| `POST /_bgh/traffic/views` | anyone | web-client page-view beacon `{owner, repo, path, referrer, title}`; always 204 (unknown/private repos are ignored silently) |

### Stats: 202 → 200

One `git log --numstat` pass (job `repos.compute_stats`) stores sparse
per-author weekly a/d/c, daily counts and the punch card in `repo_stats`,
keyed by the default-branch head SHA. A request whose head has no cached
row queues the job (deduplicated like languages) and answers **202 `{}`**;
clients retry. Empty repositories → 204 (participation: zeros). Repos
with ≥ 10,000 commits skip `--numstat` (a/d are 0, `code_frequency` 422).
When the default branch moves, post-receive re-queues the job only for
repositories whose stats were requested before. Weeks are Sunday 00:00 UTC.

### Traffic

* Clones: `traffic::CloneTap` parses upload-pack request pkt-lines (HTTP
  body after gunzip, and the SSH stdin stream): wants followed by `done`
  with no `have` = a clone (protocol v0 and v2; fetches and `ls-refs` do
  not count). Hooked in `git_http::upload_pack` and `ssh/exec.rs`.
* Visitors: `u:<user id>`, `dk:<deploy key id>` (SSH), else
  `ip:<salted sha256 of client IP>`.
* Counters are aggregated per day/visitor (`repo_traffic_views`,
  `repo_traffic_clones`); the `repos.traffic_prune` service deletes rows
  older than 31 days every 6 h (idempotent DELETE, no lock needed).

### Community profile

Health files are looked up in `.github/`, root, `docs/` (license: root
only); `.github/ISSUE_TEMPLATE/*.{md,yml}` counts as an issue template.
Health = share of description, README, code of conduct, contributing,
license, issue template, PR template (GitHub's seven). Contributor
Covenant is recognized by content (`key: contributor_covenant`), else
`other`. License: P33 (license detection) is not merged, so a LICENSE /
COPYING file reports GitHub's `Other`/`NOASSERTION`, or the repo's
`license_spdx_id` once something sets it. `files.security` (SECURITY.md)
is a Better GitHub extension used by the UI. Scan cached in Redis by tree.

### Activity log

`activity::record` runs inside post-receive (every branch update: pushes,
API ref writes, PR merges, mirror syncs; imports skipped): `branch_creation`,
`branch_deletion`, `pr_merge` (new SHA is a merged PR's merge commit on its
base), `push` (fast-forward) or `force_push`. Tags are not logged.
`merge_queue_merge` is accepted as a filter for P39.

## Tables (4300_insights.sql)

`repo_stats`, `repo_traffic_views`, `repo_traffic_clones`, `repo_activity`.

## Web

* `pages/repo/insights/InsightsPage.tsx` (one lazy chunk, ~7 KB gzip):
  Pulse (`/pulse`, `/pulse/{daily,halfweekly,weekly,monthly}`; PR/issue
  lists from the local store + commits/authors from stats), Contributors
  (`/graphs/contributors`), Community standards (`/community`), Traffic
  (`/graphs/traffic`, push access only), Commits (`/graphs/commit-activity`
  + punch card), Code frequency (`/graphs/code-frequency`), Forks (links to
  P12's `/forks`), Network (`/network/members`, fork tree two levels deep).
* `insights/charts.tsx`: hand-rolled SVG `TimeChart` (area/columns,
  negative values, crosshair tooltip, legend) and `PunchCard`, using the
  `--chart-*` tokens; `insights/data.ts` pure helpers (+ tests).
* `api/insights.ts` (stats polling on 202), `app/traffic.ts` beacon called
  from `RepoLayout` on each repo URL change.
* `nav.ts`: `graphs`/`community`/`network` select the Insights tab;
  `pulse` is no longer a placeholder tab (only `security` remains).
* Mock: `mock/insights.ts` (202 on first request per kind, deterministic
  stats, beacon-fed views, community profile) + `mock/insights.test.ts`.
* Verified with Playwright in mock mode (both themes, hover tooltip,
  390 px) and against a real server seeded by `seed-real.mjs`.

## Tests

`crates/bgh-repos/tests/it/insights.rs`: 202→200 and shapes of all five
stats endpoints, empty/private repos, recompute after push, clones over
HTTP (v2 and v0) increment counters but fetches don't, beacon + popular
paths/referrers + push-access 403, community profile (none → 100%),
activity log types/filters/pagination. Unit tests for the pkt-line tap,
log aggregation, week math and traffic buckets.

## Known gaps

* `/activity` uses page pagination, not GitHub's `before`/`after` cursors.
* Traffic beacon counts SPA navigations only (no server-rendered pages).
* Community `code_of_conduct` recognizes only the Contributor Covenant.
