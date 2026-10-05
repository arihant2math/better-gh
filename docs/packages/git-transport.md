# Package B2b: git-transport

Branch `bgh/git-transport`. Crates: `bgh-git` (git machinery) and new
module areas in `bgh-repos` (`ssh/`, `lfs/`, `browse/`, `download/`,
`maintenance.rs`), kept apart from the REST handlers of B2a. Migrations
0250-0299 (used: `0250_lfs.sql`).

## Status

Complete: all six scope items implemented with integration tests (real
`git`, `ssh` and `git-lfs` clients); benchmarks recorded below. Merged
with the integration branch (`818812f`); `cargo fmt --check`, `cargo
clippy --workspace --all-targets --locked -D warnings` and `cargo test
--workspace --locked` pass.

Tests (`crates/bgh-repos/tests/`): `browse.rs` (8), `download.rs` (4),
`lfs.rs` (5, incl. git-lfs push/clone/pull/lock), `ssh.rs` (5: clone/push
v0+v2, permissions, deploy keys, branch protection, git-lfs over SSH, host
key), `bench.rs` (ignored, env-driven); unit tests in `bgh-git`
(highlight, blame parser, LFS store, archive names, command parsing...).
SSH/git-lfs tests skip themselves when `ssh`/`git-lfs` are not installed.

## Implemented

### 1. SSH server (`bgh_repos::ssh`, russh 0.64, ring backend)
* Registered as the `ssh` service (`Registry::service`, new), started by
  `bgh serve` when `BGH_SSH_ENABLED` on `BGH_LISTEN`'s IP and
  `BGH_SSH_PORT` (default 2222). Tests use `ssh::spawn(state, addr, token)`.
* Public-key auth only. Lookup by OpenSSH fingerprint
  (`SHA256:<unpadded base64>`, `ssh::keys::fingerprint[_openssh]`) in
  `ssh_keys` (user → acts with the user's permissions, suspended users
  rejected) and `deploy_keys` (`verified`; read-only or read-write on that
  repository only). `last_used_at` is touched (≤ 1/min).
* Exec commands: `git-upload-pack` (full duplex; protocol v2 via the
  `GIT_PROTOCOL` env request), `git-receive-pack` (advertisement + the
  same `smart_http::receive_pack_stream` path as HTTP → identical branch
  protection, `repos.post_receive` job and `Event::Push`; deploy keys push
  with `pusher_id = None`), `git-lfs-authenticate <repo> upload|download`
  (JSON `{href, header: {Authorization: "RemoteAuth <token>"},
  expires_in}`). `git-lfs-transfer` / `git-upload-archive` are refused
  (git-lfs falls back to authenticate + HTTP). Shell requests print
  GitHub's "Hi {login}! ... does not provide shell access" (exit 1).
  Errors use GitHub wording (`ERROR: Repository not found.`, `Permission
  to o/r.git denied to x.`, read-only key, archived).
* Host key: Ed25519 generated on first start, persisted (0600) at
  `{data_dir}/ssh/host_ed25519_key`.

### 2. Git LFS (`bgh_repos::lfs`, `bgh_git::lfs`)
* `POST /{o}/{r}[.git]/info/lfs/objects/batch` (basic transfer,
  `hash_algo` sha256 → 409 otherwise, per-object 404/422 errors, ≤ 1000
  objects), `GET|PUT .../objects/{oid}`, `POST .../objects/{oid}/verify`.
  Media type `application/vnd.git-lfs+json`; 401s carry `LFS-Authenticate`.
* Auth: Basic (password or PAT), `token`/`Bearer`, or `RemoteAuth`
  tokens from SSH (`lfs::issue_grant`, Redis, 1 h, bound to one repo).
  Download needs read (anonymous OK on public repos), upload/verify/locks
  need write; archived repos are read-only.
* Storage: content-addressed `{data_dir}/lfs/aa/bb/<oid>`, streamed to a
  temp file with SHA-256 + size verification, shared across repos; access
  is per repository via `lfs_objects` (knowing an oid never grants
  access). `repositories.lfs_size` is maintained on link (size accounting).
* Locks API (create/list/verify/unlock, cursor pagination); unlocking
  another user's lock needs `force` + admin.
* GC: on `RepositoryDeleted`, archive cache purged and `repos.lfs_gc`
  enqueued (only if an LFS store exists) — deletes unreferenced objects
  older than 1 h.

### 3. Archives (`bgh_repos::download::archive`, `bgh_git::archive`)
* `GET /{o}/{r}/archive/{ref}.tar.gz|.tgz|.zip` (prefix `{repo}-{ref}/`,
  `/`→`-`, `v` dropped for version tags like GitHub),
  `GET /{o}/{r}/legacy.tar.gz|legacy.zip/{ref}` (prefix
  `{owner}-{repo}-{sha7}/`), REST `GET /repos/{o}/{r}/tarball|zipball[/{ref}]`
  → 302 to the legacy URL (`?token=` for private repos, 5 min).
* `git archive` output is streamed to the client while teed into
  `{data_dir}/cache/archives/{repo_id}/{commit}-{hash(prefix)}.{ext}`
  (atomic rename on success; interrupted downloads leave nothing).
  `bgh_git::archive::prune_cache` exists for age-based cleanup.

### 4. Raw (`GET /{o}/{r}/raw/{ref}/{path}`)
* Text → `text/plain; charset=utf-8` (HTML/JS never served as such),
  binaries by extension (images, pdf, media) else octet-stream, SVG as
  `image/svg+xml`; always `X-Content-Type-Options: nosniff` and a
  sandboxing CSP. Strong ETag = blob SHA; 304 support.
* Blobs over `BGH_MAX_BLOB_SIZE` stream via `git cat-file`. LFS pointers
  resolve to the stored object when linked to the repo.
* Private repos: credentials or `?token=` (`download::token::issue`, also
  usable by B2a for contents `download_url`).

### 5. Code browser endpoints (`bgh_repos::browse`, `/_bgh/repos/{o}/{r}/…`)
| endpoint | notes |
|---|---|
| `refs` | `{default_branch, branches[], tags[]}` (tags peeled, version-sorted desc) |
| `tree[/{ref}[/{path}]]` | entries (dirs first, sizes), `readme` (rendered), `last_commits` if cached |
| `tree-commits/{ref}[/{path}]` | last commit per entry (Redis `lc:` by commit+path) |
| `blob/{ref}/{path}` | `lines` (class-based highlighted HTML per line), `language`, `binary`, `image`, `mime`, `lfs`, `too_large` (> `BGH_MAX_BLOB_SIZE`), `truncated` (> 1 MiB), `rendered` (Markdown), `symlink_target`, `raw_url` |
| `blame/{ref}/{path}` | JSON, or NDJSON stream (`Accept: application/x-ndjson`) as `git blame --incremental` progresses; cached by commit+path |
| `history[/{ref}[/{path}]]` | `?page&per_page`, `has_more`; cached by commit+path+page |
| `readme[/{ref}[/{dir}]]` | rendered README |
| `/_bgh/highlight.css` | light theme + dark (`prefers-color-scheme` / `[data-theme=dark]`) |

* `{ref}` may contain slashes (`GitRepo::split_ref_path`: full SHA,
  `HEAD`, shortest branch then tag prefix, `refs/...`, abbreviated SHA).
* Full-SHA URLs: `Cache-Control: public, max-age=31536000, immutable`
  (`private, …` for private repos — never let shared caches keep private
  code) and a request-derived ETag, so revalidation and repeat requests
  skip all work. Ref URLs: `max-age=30` + content ETag. `Vary: Accept,
  Cookie, Authorization`.
* Highlighting: syntect + two-face grammars (TS/TSX, TOML, Dockerfile, …),
  Oniguruma regex engine (4.5× faster than fancy-regex here), per-line
  self-contained spans (`hl-*` classes), plain fallback above 512 KiB /
  20k lines / 2 s. Cached in Redis by blob SHA + grammar (7 days).
* Markdown: core GFM renderer; relative links → `/{o}/{r}/blob/{ref}/…`,
  relative images → `/{o}/{r}/raw/{ref}/…` (new `markdown::rewrite_urls`).
* Commit authors map to accounts by verified email (one batched query).
* Last commit per entry (`GitRepo::last_commits`): date-ordered walk with
  TREESAME pruning; an entry is attributed to the newest commit that
  introduced its current object id; trees memoized per walk.
* After a push to the default branch the root last-commit map is warmed
  in-process (`maintenance::on_event`).

### 6. Repository handle cache + maintenance
* `bgh_git::cache`: process-wide LRU (256) of `gix::ThreadSafeRepository`
  keyed by path; `RepoStore::open/read` use it transparently (gix now
  built with `parallel`). Evicted on `RepoStore::delete`.
* `repos.pack_refs` job (enqueued after a push when ≥ 64 loose refs):
  `git pack-refs --all --prune` so ref listings use packed, pre-peeled refs.

## Benchmarks

Fixture: bare clone of `tokio-rs/tokio` (4 753 commits, 883 files, 476
refs incl. 398 tags, 20 MB) at `b263675`, served through the full HTTP
stack in-process (`tests/bench.rs`, release build, 4-core Xeon 2.8 GHz,
local Postgres/Redis). "cold" = Redis caches flushed before each request
(git + compute), "warm" = cache hit. Full-SHA URLs; browsers additionally
skip warm requests entirely (immutable caching / 304).

| endpoint | cache | p50 ms | p90 ms |
|---|---|---|---|
| refs (loose refs) | – | 9.5 | 10.1 |
| refs (after `pack-refs`) | – | 3.3 | 3.7 |
| tree root | cold | 4.1 | 9.0 |
| tree root | warm | 1.4 | 1.5 |
| tree tokio/src/runtime | warm | 1.2 | 1.3 |
| tree-commits root (last commit per entry) | cold | 157.8 | 189.7 |
| tree-commits root | warm | 2.2 | 3.0 |
| tree-commits tokio/src/runtime | cold | 116.2 | 118.9 |
| tree-commits tokio/src/runtime | warm | 1.3 | 1.8 |
| blob runtime/builder.rs (82 KB, highlighted) | cold | 98.3 | 137.5 |
| blob runtime/builder.rs | warm | 6.5 | 8.5 |
| blame runtime/builder.rs | cold | 132.8 | 163.1 |
| blame runtime/builder.rs | warm | 3.3 | 3.9 |
| history runtime/builder.rs (130 commits) | cold | 23.5 | 24.2 |
| history runtime/builder.rs | warm | 1.7 | 2.2 |
| raw runtime/builder.rs | – | 1.6 | 1.9 |

Repo handle cache: open + list branches p50 0.24 ms cached vs 0.43 ms
uncached. Optimizations driven by these runs: syntect on Oniguruma
(blob cold 396 → 98 ms), ref listing without per-ref `peel_to_id`
(tags 4.6 → 0.6 ms), post-push `pack-refs` job (refs 9.5 → 3.3 ms),
post-push warm-up of the root last-commit map.

Reproduce:
```
git clone --bare https://github.com/tokio-rs/tokio.git /tmp/tokio.git
BGH_BENCH_REPO=/tmp/tokio.git BGH_BENCH_DIR=tokio/src/runtime \
BGH_BENCH_FILE=tokio/src/runtime/builder.rs \
  cargo test --release -p bgh-repos --test bench -- --ignored --nocapture
```

## Tables / migrations
* `0250_lfs.sql`: `lfs_objects(repo_id, oid, size, uploader_id,
  created_at)` PK `(repo_id, oid)` + index on `oid`; `lfs_locks` (unique
  `(repo_id, path)`, indexes on `(repo_id, id)` and `owner_id`);
  `repositories.lfs_size BIGINT NOT NULL DEFAULT 0`.

## Shared-code changes (all additive)
* Workspace deps: `russh` (ring), `syntect` (onig), `two-face`; `gix`
  gains the `parallel` feature (thread-safe handles for the cache).
* `bgh_core::registry`: `Registry::service`, `Service`, `spawn_services`
  (`bgh serve` starts them; the test harness does not).
* `bgh_core::markdown`: `rewrite_urls`, `UrlAttr`.
* `bgh-server`: `main.rs` spawns services; compression skips
  `application/zip`, `application/x-gzip`, `application/octet-stream`.
* `bgh-git`: new modules `archive`, `blame`, `cache`, `highlight`,
  `lastcommit`, `lfs`, `maintenance`, `stream`; `GitRepo::open_cached`,
  `split_ref_path`, `git_bin`; `smart_http::receive_pack_stream`,
  `PushResult`, `advertise_refs`, `upload_pack_duplex`, `valid_protocol`
  (`receive_pack` now delegates; the rejected-push drain has a 10 s idle
  timeout and git's stdin feeder is aborted once git exits).
* `bgh-repos/protection.rs`: `check_push_by(rules, access, Option<user_id>, updates)`
  (`check_push` delegates) for deploy keys.

## Notes for other packages
* B1 (accounts): store `ssh_keys.fingerprint` as OpenSSH SHA256 (use
  `bgh_repos::ssh::keys::fingerprint_openssh(line)` or the same format).
  Deploy keys need `verified = true` to authenticate.
* B2a (repos API): REST `tarball`/`zipball` routes are implemented here
  (`download::api_router`) — don't add them again (axum panics on duplicate
  routes). For contents `download_url` of private repos use
  `download::token::issue`. `GET /_bgh/...` browse routes are in
  `browse::web_router`.

## Known gaps / TODO
* Force-push detection in branch protection still needs the objects
  (quarantine pre-receive) — unchanged from the HTTP path.
* No concurrent-request coalescing for cold computations (two first
  visitors compute the same last-commit map twice).
* Archive cache has no automatic age-based pruning yet
  (`archive::prune_cache` exists; needs a periodic trigger, e.g. B7 admin
  maintenance).
* LFS: no per-repo quota enforcement (size is accounted), no `Range`
  downloads, no SSH `git-lfs-transfer` protocol.
* SSH: no OpenSSH certificate auth, no `git-upload-archive`.
