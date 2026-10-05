# P2 push-hardening — status

**Done.** Branch `bgh/p02-push-hardening`. No migrations (range 1400–1499
unused).

## Hidden refs

* `REPO_CONFIG` (`bgh-git/src/storage.rs`, now versioned:
  `CONFIG_VERSION = 2`, stamped `bgh.configVersion`) sets
  `receive.hideRefs = refs/pull/` and `refs/bgh/`.
* Defense in depth: every `git receive-pack` (push and its advertisement,
  HTTP and SSH) also gets `-c receive.hideRefs=...`, independent of the
  on-disk config; `git_http::deny_hidden_refs` (HTTP + SSH authorize
  callbacks) refuses a push whose updates are *all* hidden before git
  runs. Mixed pushes (`git push --mirror` of a GitHub clone) reach git,
  which rejects just the hidden refs (`deny updating a hidden ref`), so
  branches still update.
* REST: `refs::write_ref` (used by `POST/PATCH/DELETE /git/refs`) answers
  422 `Reference update failed: <ref> is a hidden ref.` Reads still work;
  fetch/ls-remote still advertise `refs/pull/*` like GitHub.
* Internal writers (`bgh_git::merge::force_ref` / `fetch_ref`, used by
  `bgh-pulls` `mirror_head` and test merges) don't go through
  receive-pack and are unaffected (tested).

## fsck

* `receive.fsckObjects = true` in `REPO_CONFIG`; per push the site setting
  `git.fsck_on_push` (default true) is passed as `-c
  receive.fsckObjects=...`. `receive.fsck.{zeroPaddedFilemode,badTimezone,
  missingSpaceBeforeDate} = ignore`; `.gitmodules` (`gitmodulesUrl`,
  `gitmodulesName`, ...) and symlink checks stay fatal (tested: option
  injection URL and `../` submodule name are rejected).

## Config upgrade

* `RepoStore::upgrade_config` / `upgrade_all_configs` (pure Rust text
  rewrite: drops every variable `REPO_CONFIG` sets, drops sections left
  empty, appends `REPO_CONFIG`; atomic rename; idempotent; foreign keys
  such as `core.bare` kept). Walks `{repos}/xx/*` so wikis are included.
* `bgh_repos::maintenance::upgrade_repo_configs` + service
  `repos.config_upgrade` (runs once at `bgh serve` startup; no lock
  needed: idempotent, atomic per file). Repos already at the version cost
  one small file read.
* `init`/`fork` now write the config through the same rewrite.

## Size limits and quotas

* Site settings section `git` (`bgh_core::settings::GitSettings`, in
  `SECTIONS`): `fsck_on_push` (true), `max_object_size_mb` (100),
  `warn_object_size_mb` (50), `max_push_size_mb` (2048); `null` disables
  a limit. Admin validation: positive, warn < max.
* `smart_http::PushPolicy` gained `limits: PushLimits` (fsck override,
  blob limits, `receive.maxInputSize`, remaining quota). `PRE_RECEIVE_HOOK`
  now (1) compares `du -sk $GIT_QUARANTINE_PATH` with the remaining quota,
  (2) runs `rev-list --objects <new> --not --all | cat-file --batch-check`
  and rejects blobs over the limit with GitHub's GH001 text / warns above
  the warn size (sideband `remote:` lines), (3) the existing protection
  checks. The hook now runs on every push with limits configured.
* `bgh_core::settings::quota_headroom` (new; per-repo and owner-total,
  counting `size + lfs_size`) backs `check_push_quota` (now LFS-aware;
  still a 403 up front when already over, as `bgh-admin` tests expect)
  and the hook's quarantine check (push that would overshoot → rejected
  before refs change).
* LFS: batch `upload` returns 507 (LFS JSON) when the objects it would
  add don't fit; direct `PUT` checks too (declared length up front,
  unsized uploads after hashing; the unlinked object is collected by LFS
  gc).

## Web

* Site admin → Settings → **Git pushes** section (`GitSection` in
  `settingsSections.tsx`, form model in `settingsForm.ts` with unit test).
  Verified with Playwright against a real server (save, client
  validation). Admin endpoints are not part of the mock backend.

## Shared-code changes (additive)

* `bgh-core/src/settings.rs`: `GitSettings`, `"git"` section,
  `QuotaHeadroom`, `quota_headroom`; `check_push_quota` reimplemented on
  top of it (same messages).
* `bgh-git`: `storage::{CONFIG_VERSION, HIDDEN_REF_PREFIXES,
  is_hidden_ref, config_version, rewrite_config}`,
  `RepoStore::{upgrade_config, upgrade_all_configs}`,
  `smart_http::{PushLimits, PushPolicy::limits, with_limits,
  HIDDEN_REF_REASON}`. Later packages that enumerate pushed objects (P23,
  P65) can extend `PRE_RECEIVE_HOOK` the same way.

## Tests

* `bgh-repos` `tests/it/push_hardening.rs`: hidden refs over HTTP
  (single + mirror push, internal writers), refs API 422s, fsck
  `.gitmodules` + setting off, GH001 101 MB reject / 60 MB warn + setting,
  `receive.maxInputSize`, quota (incoming push, LFS batch 507, LFS PUT
  507, LFS usage blocks git pushes), config upgrade.
* `tests/it/ssh.rs::push_hardening_over_ssh`: hidden refs + GH001 over
  SSH (skips without `ssh`).
* `bgh-git` unit tests: config rewrite idempotence, hidden ref matching,
  upgrade over a store.

## Known gaps

* Wiki pushes (`bgh_wiki::git`) get hidden refs and fsck (config + `-c`)
  but not the size/quota hook.
* The quota pre-check uses on-disk quarantine size (compressed), the same
  measure as `repositories.size`; LFS objects are counted in bytes.
