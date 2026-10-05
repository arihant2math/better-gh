# P1 git-maintenance — status

**Done.** Branch `bgh/p01-git-maintenance`. Scope: `docs/PHASE4_PLAN.md`
§2 P1 (no §5 quick fixes are assigned to P1). Migration: `1300`.

## What changed

### `bgh_git::maintenance` (fork-network aware)

* `NetworkRole { has_alternates, has_dependents }` and the pure
  `plan(task, role, grace_days)` → git invocations (unit-tested), executed
  by `run_task` (evicts the gix handle cache afterwards):

  | Task | dependents (parent / middle fork) | fork, no dependents | standalone |
  |---|---|---|---|
  | `Gc` | `repack -a -d --keep-unreachable` (+`-l` if it has alternates) | `repack -A -d -l --unpack-unreachable=<grace>` + `prune --expire <grace>` | `gc --prune=<grace>` |
  | `Repack` | `repack -a -d --keep-unreachable` (`-l` with alternates) | same | same |
  | `Incremental` | `commit-graph` + `repack -d --geometric=2` (`-l` with alternates, else `--write-midx --write-bitmap-index`) | same | same |
  | `CommitGraph` | `commit-graph write --reachable --changed-paths [--split]` | | |
  | `PruneNow` | **refused** (`PlanError::HasDependents`) | `repack -a -d -l` + `prune --expire now` | `repack -a -d` + `prune --expire now` |

  `Gc`/`Repack` also run `pack-refs` and write a commit-graph. Forks write
  a self-contained (non-split) commit-graph: a split chain in a fork
  references the parent's layers, which the parent may later merge away.
  With alternates `--no-write-bitmap-index` is passed (bare repos write
  bitmaps by default, impossible with alternates).
* Deviation from the plan text, on purpose: a fork without dependents is
  repacked with `-A --unpack-unreachable=<grace>` + `prune --expire
  <grace>` instead of plain `repack -a -d -l`, so its unreachable objects
  also get the grace period (an in-flight push's objects are never
  dropped).
* Dependents: `dependents_index(root)` / `dependents_on_disk` scan every
  `objects/info/alternates` under the store (the filesystem fallback);
  callers combine it with DB `parent_id`.
* `dissociate(git_dir)`: `repack -a -d --keep-unreachable`, park the
  alternates file, `fsck --connectivity-only`; on failure the alternates
  file is restored and an error returned. `dissociate_network` does the
  dependents of a repository first (deepest first): a fork of a fork may
  need parent objects its middle fork no longer references.
  `dissociate_dependents` = all repositories borrowing from a directory.
* `object_stats` (`count-objects -v`), `push_in_progress` (git's
  quarantine dir `objects/tmp_objdir-incoming-*` younger than 1 h),
  `alternates`, `has_alternates`.
* Unit test `no_immediate_prune_in_codebase` fails if the string
  `prune` + `=now` appears anywhere under `crates/`.

### `bgh_git::archive`

* New `prune_cache_to_size(cache_dir, max_bytes)` (oldest first), next to
  the existing `prune_cache` (by age), both now called by the scheduler.

### `bgh_repos::maintenance`

* `AdvisoryLock` (session-level pg advisory lock on a dedicated pooled
  connection; dropped without `release()` → the connection is detached and
  closed so the lock can't leak) and `lock_repo(state, id, wait)`.
* `network_role(state, id)`: alternates from disk, dependents from DB or
  disk.
* `run_task(state, id, task)`: waits for the repo lock, runs the plan,
  records `repo_maintenance` (gc/repack/prune count as a full run).
* `dissociate(state, id)`: `dissociate_network` under the repo lock,
  then records fresh stats.
* `service` = `reg.service("repos.maintenance")`: every 60 s while
  `git_maintenance.enabled`, `run_pass` takes the leader advisory lock
  (`pg_try_advisory_lock`, so one server runs it) and picks due repos
  (never maintained; pushed since the last run; full repack due; failed
  ones back off for `interval_hours`), ordered by `last_run_at`, at most
  `max_repos_per_pass`. Per repo: skip while a push is in progress or the
  repo lock is held; `Gc` when the full repack is due, else `Incremental`
  when loose objects ≥ threshold, packs ≥ threshold or `interval_hours`
  passed, else `CommitGraph`. Archive cache pruning at most hourly.
  Returns a `PassReport` (used by tests).
* Job `repos.maintenance_pass` (`RunPass`) = one pass now (admin "Run
  now").
* `jobs::delete_storage`: dissociates every DB fork (and their forks) and
  then anything else on disk still borrowing from the deleted repo,
  before removing the directory.

### Admin (`bgh-admin`)

* `POST /_bgh/admin/repos/{o}/{r}/maintenance`: `gc` / `repack` now call
  `bgh_repos::maintenance::run_task`. New operations: `prune` (needs
  `"force": true`, 422 on a repository with dependents; audited with
  `force: true` in the `repo.maintenance` entry) and `dissociate`.
  `POST /_bgh/admin/maintenance` (every repo) rejects `prune` and
  `dissociate` with 422.
* `POST /_bgh/admin/repos/{o}/{r}/detach` → 202 run ("leave fork
  network"): clears `fork`/`parent_id`/`source_id`, decrements the
  parent's `forks_count`, makes the repo the `source` of its own fork
  subtree (sync actions recorded), audits `repo.detach_fork_network`, and
  queues a `dissociate` run. 422 if the repo isn't in a fork network.
* `GET /_bgh/admin/git-maintenance` → `{settings, repositories, succeeded,
  failed, skipped, never_run, with_dependents, last_run_at}`.
* `GET /_bgh/admin/git-maintenance/repos?status=` → paginated per-repo
  state (failed first).
* `POST /_bgh/admin/git-maintenance/run` → 202 `{"queued": true}`.
* `GET /_bgh/admin/repos/{o}/{r}` adds `git_maintenance` (row or null)
  and `network {has_alternates, has_dependents}`.
* Settings validation for the new section.

### Site settings (shared, additive)

* `bgh_core::settings::GitMaintenanceSettings`, section `git_maintenance`
  (in `SECTIONS`): `enabled` (true), `prune_grace_days` (14),
  `interval_hours` (24), `full_interval_days` (7),
  `loose_objects_threshold` (1000), `pack_count_threshold` (16),
  `max_repos_per_pass` (20), `archive_cache_max_age_days` (7),
  `archive_cache_max_size_mb` (2048). Edited through the existing
  `PATCH /_bgh/admin/settings`.

### Migration `1300_git_maintenance.sql`

* `repo_maintenance_runs.operation` check constraint widened with
  `prune`, `dissociate` (drop + re-add; the old migration is untouched).
* New `repo_maintenance` (`repo_id` PK → repositories ON DELETE CASCADE,
  `last_run_at`, `last_full_at`, `status` pending/succeeded/failed/
  skipped, `error`, `pack_count`, `loose_count`, `has_alternates`,
  `has_dependents`, `updated_at`) + indexes on `(status, updated_at)` and
  `last_full_at`.

### Web

* New page **Site admin → Git maintenance** (`/site-admin/maintenance`,
  `g m`): status tiles, schedule form (`git_maintenance` settings,
  validated client-side), "Run now", per-repo status table with status
  tabs; rows link to the repo admin page.
* Repo admin page, Maintenance panel: network role badge (Standalone /
  Fork / Fork parent / Fork with forks) and the scheduled status line;
  "Prune now…" (typed confirmation, disabled with a reason when forks
  borrow from the repo); "Leave fork network…" for forks. gc/repack
  descriptions updated.
* `SiteSettings` type gains `git_maintenance`; the settings page's
  `SectionKey` excludes it (edited on its own page).
* Verified with Playwright against a real server (schedule edit +
  validation, prune disabled on a fork parent, detach flow → "Not a fork",
  `dissociate` run listed).

### Docs

* `docs/SELF_HOSTING.md`: backup ordering text corrected (objects are
  removed after the grace period; parents never pruned; archive cache can
  be excluded) + a "Git maintenance" section.
* `docs/ARCHITECTURE.md` Git section: dissociation and fork-aware
  maintenance.

## Tests

* `crates/bgh-repos/tests/it/maintenance.rs`:
  * `fork_network_survives_parent_maintenance`: alice/lib ← bob/lib ←
    carol/lib with all objects packed in alice; alice deletes x and y,
    bob deletes x; admin gc + repack on alice and bob, forced prune
    refused (also site-wide, also when the DB lost `parent_id` — the
    filesystem fallback), two scheduled passes (full, then incremental
    after a push). bob and carol pass `fsck --connectivity-only`, clone
    over HTTP, commits/contents/compare and PR create + files APIs work on
    x. Commit-graphs exist; forks have no bitmaps. Verified to fail with a
    plain `repack -a -d` on the parent.
  * `deleting_parent_makes_forks_self_contained`: no alternates left,
    fsck clean, clones and APIs work (including the fork of the fork).
  * `detach_from_fork_network`: REST shape, subtree source, forks count,
    dissociation, then a forced prune of the old parent harms nothing.
  * `push_during_repack_succeeds`: four pushes racing gc / incremental
    runs; pushes succeed, refs match, fsck clean.
  * `scheduled_pass_on_standalone_repo_and_archive_cache`: "Run now"
    (403 for non-admins), old archives pruned, bitmap on the standalone
    repo, incremental pass, nothing-due pass, forced prune audited,
    overview / status listing shapes, settings validation.
* `bgh-git` unit tests: plans per role, grace, count-objects parsing,
  the `prune` + `=now` grep.

## Known gaps / notes

* Bitmaps: written for every repository without alternates, including
  fork parents (the plan's scope says "only for repos without
  alternates"; the acceptance line "a bitmap exists only on a repo
  without forks" is read as "not a fork"). Forks never get one.
* Wiki repositories are not scheduled yet (unchanged: no automatic
  maintenance for wikis).
* Repositories with dependents keep unreachable objects forever (as on
  GitHub); a network-level object store would be needed to reclaim them.
* Leaving a fork network or deleting a parent copies objects into every
  descendant fork (correctness first; no re-sharing).
* The admin endpoints are not in `web/src/mock/` (no site-admin endpoint
  is mocked today).
* Skip-on-push uses git's quarantine directory, which covers HTTP and SSH
  pushes in any process; a push starting right after the check is still
  safe because nothing is pruned before the grace period.
