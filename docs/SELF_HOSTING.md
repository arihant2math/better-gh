# Self-hosting Better GitHub

Better GitHub is one binary, `bgh`, plus PostgreSQL and Redis. The binary
serves the REST API (`/api/v3`), GraphQL (`/api/graphql`), git smart HTTP,
the web client, and runs background jobs. Database migrations are embedded
in the binary and applied on start.

| Component  | Version                     | Holds                                         |
|------------|-----------------------------|-----------------------------------------------|
| `bgh`      | this repository             | stateless apart from `BGH_DATA_DIR`           |
| PostgreSQL | 16 recommended (CI uses 16) | all metadata, job queue, sync log, audit log  |
| Redis      | 7 recommended               | caches, rate limits, pub/sub only (no backup needed) |
| git        | ≥ 2.38 (`merge-tree --write-tree`) | invoked by `bgh` for transport and writes |
| git-lfs    | optional                    | LFS-enabled clients                           |

Ports: **3000** HTTP (API, web, git over HTTP), **2222** git over SSH
(advertised in `ssh_url`; see [SSH](#ssh)).

Contents: [Docker Compose](#quick-start-docker-compose) ·
[Binary + systemd](#install-binary--systemd) ·
[Configuration reference](#configuration-reference) ·
[First admin](#creating-the-first-admin) ·
[Reverse proxy and TLS](#reverse-proxy-and-tls) · [SSH](#ssh) ·
[Email](#email-smtp) · [Backup and restore](#backup-and-restore) ·
[Upgrades](#upgrades-and-migrations) · [Operations](#operations)

## Quick start (Docker Compose)

```sh
git clone <this repository> bgh && cd bgh
cat > .env <<'EOF'
BGH_BASE_URL=http://localhost:3000
POSTGRES_PASSWORD=change-me
EOF
docker compose up -d --build
docker compose exec bgh bgh admin create-user \
  --login octo --email octo@example.com --password 'a long passphrase' --site-admin
open http://localhost:3000
```

`docker-compose.yml` runs `postgres:16-alpine`, `redis:7-alpine` and the
`bgh` image built from `Dockerfile`. Data lives in the named volumes
`pgdata` (database) and `bghdata` (`/data`: git repositories, uploads).
Variables from `.env` that the compose file passes through:
`BGH_BASE_URL`, `POSTGRES_PASSWORD`, `BGH_SSH_PORT`, `BGH_SIGNUP_ENABLED`,
`BGH_SITE_NAME`, `BGH_JOB_WORKERS`, `RUST_LOG`, `BGH_HTTP_PUBLISH` /
`BGH_SSH_PUBLISH` (host ports, default 3000 / 2222) and `BGH_IMAGE`. Add
other settings from the [reference](#configuration-reference) under
`services.bgh.environment`.

### The image

The `Dockerfile` is multi-stage: Node builds `web/`, cargo-chef caches the
Rust dependency build, and the runtime stage is `debian:trixie-slim` with
`git`, `git-lfs`, `ca-certificates` and `tini`:

* runs as the non-root user `bgh` (uid/gid 10001); `/data` is a volume
  owned by it (`BGH_DATA_DIR=/data`)
* the web client is compiled into the binary (cargo feature `embed-web`),
  so the image needs no other files
* `HEALTHCHECK` runs `bgh healthcheck` (HTTP `GET /healthz`, which also
  checks the database and Redis)
* `ENTRYPOINT ["tini", "--", "bgh"]`, default command `serve`, so
  `docker run IMAGE migrate` or `docker run IMAGE admin create-user ...`
  work too

Build arguments: `RUST_VERSION` (1.97), `NODE_VERSION` (22),
`DEBIAN_RELEASE` (trixie), `CARGO_FEATURES` (`embed-web`; set to `""` to
serve the client from `BGH_WEB_DIR` instead), `CARGO_BUILD_JOBS` (cap
parallel rustc processes on small builders; the release profile uses LTO
and one codegen unit, so the final link needs a few GB of RAM).

```sh
docker build -t bgh .
docker run -d --name bgh -p 3000:3000 -p 2222:2222 -v bgh-data:/data \
  -e DATABASE_URL=postgres://bgh:...@db.internal/bgh \
  -e REDIS_URL=redis://redis.internal/ \
  -e BGH_BASE_URL=https://git.example.com bgh
```

## Install (binary + systemd)

Build a release binary with the web client embedded:

```sh
(cd web && npm ci && npm run build)       # -> web/dist
cargo build --release --locked -p bgh-server --bin bgh --features embed-web
install -m 0755 target/release/bgh /usr/local/bin/bgh
```

Without `--features embed-web` the binary serves the client from
`BGH_WEB_DIR` (copy `web/dist` there). With it, a `BGH_WEB_DIR` that
contains an `index.html` still takes precedence, which lets you hot-swap a
client build without rebuilding the server.

Prepare PostgreSQL and Redis (any reasonably recent distribution packages
work), then:

```sh
sudo -u postgres createuser --pwprompt bgh
sudo -u postgres createdb --owner bgh bgh
useradd --system --home-dir /var/lib/bgh --shell /usr/sbin/nologin bgh
install -d -m 0750 /etc/bgh
install -m 0640 -g bgh deploy/bgh.env.example /etc/bgh/bgh.env   # edit it
install -m 0644 deploy/systemd/bgh.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now bgh
journalctl -u bgh -f
```

`deploy/systemd/bgh.service` keeps data in `/var/lib/bgh`
(`StateDirectory`), sends SIGTERM to `bgh` only (`KillMode=mixed`, so
in-flight pushes finish during graceful shutdown) and applies systemd
sandboxing (`ProtectSystem=strict`, no capabilities, ...). `systemd-analyze
security bgh` rates it 3.0 ("OK").

## Configuration reference

Configuration is read from environment variables only (`bgh_core::config`,
`crates/bgh-core/src/config.rs`); there is no config file. Empty values
count as unset. Invalid values (e.g. a non-numeric port) stop the server at
start-up with an error naming the variable.

| Variable | Default | Description |
|----------|---------|-------------|
| `BGH_BASE_URL` | `http://localhost:3000` | External URL exactly as users reach it (scheme, host, optional port; trailing `/` is stripped). Every generated link uses it: API `url`/`html_url`/`*_url` fields, `clone_url`, the host in `ssh_url`. An `https://` base URL also marks session cookies `Secure`. **Set this in production.** |
| `BGH_LISTEN` | `0.0.0.0:3000` | Socket address the HTTP server binds. Use `127.0.0.1:3000` behind a reverse proxy on the same host. |
| `DATABASE_URL` | `postgres://postgres:postgres@localhost/bgh` | PostgreSQL connection URL (`?sslmode=require` etc. supported). The database must exist; tables are created by migrations. |
| `BGH_DB_MAX_CONNECTIONS` | `20` | Size of the PostgreSQL connection pool (per `bgh` process). |
| `REDIS_URL` | `redis://127.0.0.1/` | Redis connection URL (`redis://[:password@]host[:port][/db]`; TLS `rediss://` is not compiled in). |
| `BGH_REDIS_PREFIX` | `bgh:` | Prefix for every Redis key and pub/sub channel. Lets several instances share one Redis. (Unlike other variables, an empty value means "no prefix".) |
| `BGH_DATA_DIR` | `./data` | Persistent data: bare git repositories in `repos/` (sharded as `repos/{id % 256}/{id}.git`), uploads in `files/` (release assets, comment attachments in `files/attachments/`). Back this up. |
| `BGH_WEB_DIR` | `web/dist` | Directory of the built web client. Binaries built with `embed-web` only use it when it contains an `index.html`. |
| `BGH_SSH_PORT` | `2222` | Port advertised in `ssh_url` (and bound by the SSH server once it ships). Set to the port users connect to (e.g. `22` when a proxy/NAT forwards 22 → 2222). |
| `BGH_SSH_ENABLED` | `true` | Enable git over SSH (no effect until the SSH server ships, see [SSH](#ssh)). |
| `BGH_SIGNUP_ENABLED` | `true` | Allow self-service sign-up. The first account ever created becomes site admin, so create the admin first (or keep this `false` and use `bgh admin create-user`). |
| `BGH_SESSION_TTL_DAYS` | `30` | Lifetime of web sessions. |
| `BGH_JOB_WORKERS` | `4` | Concurrent background job workers per process (webhooks, post-receive, repo deletion, ...). `0` disables job processing in this process. |
| `BGH_GIT_BIN` | `git` | git executable (≥ 2.38). |
| `BGH_MAX_BLOB_SIZE` | `10485760` (10 MiB) | Larger blobs are not loaded into memory by API/rendering code (they are still served raw and over git). |
| `BGH_SITE_NAME` | `Better GitHub` | Instance name shown in the UI. |
| `BGH_SMTP_URL` | unset | SMTP relay for outgoing mail (`smtps://user:pass@host:465`, `smtp://host:25?tls=required`). Unset: mail is logged and written to `{BGH_DATA_DIR}/mail/`. The admin `smtp` site setting, when enabled, takes precedence. See [Email](#email-smtp). |
| `BGH_MAIL_FROM` | `{site name} <noreply@{host}>` | `From:` address of outgoing mail (when the `smtp` site setting doesn't set one). |
| `BGH_TRUST_PROXY` | `false` | Take client IPs from `X-Forwarded-For` / `X-Real-IP` (audit log, anonymous rate limits). Enable only behind a reverse proxy. |
| `BGH_RATE_LIMIT_ENABLED` | `false` | Enforce API rate limits (403 + `Retry-After` when exhausted). Budgets are always counted and reported in `X-RateLimit-*` and `GET /api/v3/rate_limit`. |
| `BGH_RATE_LIMIT` / `BGH_RATE_LIMIT_ANONYMOUS` | `5000` / `60` | REST (`core`) requests per hour per user / per client IP. `BGH_RATE_LIMIT=0` also turns enforcement off. |
| `BGH_RATE_LIMIT_SEARCH` / `BGH_RATE_LIMIT_SEARCH_ANONYMOUS` | `30` / `10` | Search requests per minute per user / per client IP. |
| `BGH_RATE_LIMIT_GRAPHQL` | `5000` | GraphQL requests per hour per user (anonymous: the `core` anonymous budget). |
| `BGH_OIDC_ISSUER`, `BGH_OIDC_CLIENT_ID`, `BGH_OIDC_CLIENT_SECRET`, `BGH_OIDC_ID`, `BGH_OIDC_NAME`, `BGH_OIDC_SCOPES`, `BGH_OIDC_AUTO_CREATE`, `BGH_OIDC_LOGIN_CLAIM`, `BGH_OIDC_ALLOWED_DOMAINS` | unset | One OpenID Connect sign-in provider (issuer and client id required); see `bgh_accounts::sso`. |

Site admins can change rate limits, SMTP and sign-in providers at runtime
(`/_bgh/admin/settings`); a field stored there overrides the variable,
fields never set keep following it.

Also read by the binary:

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Log filter (default `info,sqlx=warn,tower_http=info`), e.g. `debug`, `info,bgh_repos=debug`. Logs go to stderr. |
| `BGH_ADMIN_PASSWORD` | Password for `bgh admin create-user` when `--password` is omitted (keeps it out of shell history and `ps`). |
| `HOSTNAME` | Prefix of the worker ids that job workers record on locked jobs (default `bgh`). |

Booleans accept `1/true/yes/on` and `0/false/no/off`.

## Creating the first admin

```sh
# Docker Compose
docker compose exec -e BGH_ADMIN_PASSWORD='a long passphrase' bgh \
  bgh admin create-user --login octo --email octo@example.com --site-admin
# (there is no interactive prompt: pass --password or set BGH_ADMIN_PASSWORD)

# systemd install: run as the service user with the service's environment
# (quote values containing spaces in bgh.env, e.g. BGH_SITE_NAME="Acme Git")
sudo -u bgh BGH_ADMIN_PASSWORD='a long passphrase' sh -c 'set -a && . /etc/bgh/bgh.env &&
  exec bgh admin create-user --login octo --email octo@example.com --site-admin'
```

* Logins follow GitHub's rules (alphanumerics and hyphens, no leading or
  trailing hyphen, ≤ 39 characters) and names used by routes are reserved,
  e.g. `admin`, `api`, `login`, `settings`, `user` (see `RESERVED_LOGINS` in
  `crates/bgh-accounts/src/validate.rs`).
* Admin commands apply pending migrations first, so they work on an empty
  database.
* Alternatively the **first account that signs up** in the web UI becomes
  site admin; disable sign-up afterwards with `BGH_SIGNUP_ENABLED=false`.
* Organizations: `bgh admin create-org --login acme --admin octo [--name "Acme"]`.
* API tokens for automation: `bgh admin create-token --user octo --scopes repo,read:org`
  prints a personal access token (`bghp_…`) to stdout. Users create their
  own tokens in the web UI.

## Reverse proxy and TLS

`bgh` speaks plain HTTP; terminate TLS in a reverse proxy and set
`BGH_BASE_URL=https://git.example.com` and `BGH_LISTEN=127.0.0.1:3000`.
Requirements for the proxy:

* **WebSocket upgrade** on `/_bgh/sync/ws` (the web client's live sync),
  with a long read timeout (≥ 1 h).
* **No request body size limit** (git pushes, LFS objects and release
  assets can be large).
* **No buffering for git**: stream request and response bodies of
  `/{owner}/{repo}(.git)/info/refs`, `git-upload-pack`, `git-receive-pack`
  and `info/lfs/*`, with long timeouts.
* Forward `Host`, `X-Forwarded-For` (recorded with sessions) and
  `X-Forwarded-Proto`.
* Don't re-compress: `bgh` compresses responses and serves precompressed
  static assets itself.

Ready-made configs: [`deploy/nginx/bgh.conf`](../deploy/nginx/bgh.conf)
(certificates from certbot/acme.sh; includes an optional `stream {}` block
for SSH on port 22) and [`deploy/caddy/Caddyfile`](../deploy/caddy/Caddyfile)
(automatic Let's Encrypt certificates). Both pass `nginx -t` /
`caddy validate`.

The `gh` CLI and other GitHub Enterprise clients require HTTPS. Point
`gh` at the instance with `GH_HOST=git.example.com` and
`GH_ENTERPRISE_TOKEN=<personal access token>` (or `gh auth login --hostname
git.example.com --with-token`, which also needs the GraphQL API).
`scripts/gh-compat.sh --url https://git.example.com --token …` reports
which `gh` commands work against an instance.

## SSH

`BGH_SSH_PORT` (default 2222) is advertised in repository `ssh_url`s and is
exposed by the image and compose file. **Status:** the built-in SSH server
(russh, keys from user SSH keys and deploy keys) is being built in the
repos package and is not listening yet in this version; use HTTPS remotes
until then. Once it ships:

* publish the port directly (`2222`), or forward 22 → 2222 (nginx `stream`
  block in `deploy/nginx/bgh.conf`, a NAT rule, or bind 22 directly with
  `BGH_SSH_PORT=22` and `CAP_NET_BIND_SERVICE`, see the systemd unit) and
  set `BGH_SSH_PORT` to the port users connect to;
* HTTP reverse proxies such as Caddy cannot carry SSH.

## Email (SMTP)

Account mail (email verification, password reset, invitations, security
notices) and notification emails are queued as `mail.send` jobs and sent
through, in order of precedence:

1. the `smtp` site setting (host, port, TLS mode, credentials, `From:`),
   when a site admin enabled it in the admin settings;
2. `BGH_SMTP_URL` (with `BGH_MAIL_FROM`);
3. otherwise the development transport: messages are logged and written to
   `{BGH_DATA_DIR}/mail/` (`.eml`), nothing leaves the host.

Failed deliveries are retried with backoff by the job queue (visible in
the admin job inspector).

## Backup and restore

What to back up:

1. **PostgreSQL** — the `bgh` database (all metadata).
2. **`BGH_DATA_DIR`** — git repositories (`repos/`) and uploaded files
   (`files/`).
3. Your configuration (`/etc/bgh/bgh.env` or `.env`).

Redis holds only caches and pub/sub and needs no backup.

Take the database dump **first**, then copy the data directory: git objects
are only ever added (automatic gc is off), so every commit the database
refers to still exists in a slightly newer copy of the repositories, while
the opposite order can leave the database pointing at objects that were
never copied.

```sh
# binary install
pg_dump --format=custom --file=bgh-$(date +%F).dump "$DATABASE_URL"
tar -C /var/lib/bgh -czf bgh-data-$(date +%F).tar.gz .

# Docker Compose
docker compose exec -T postgres pg_dump -U bgh --format=custom bgh > bgh-$(date +%F).dump
docker run --rm -v bgh_bghdata:/data:ro -v "$PWD":/backup debian:trixie-slim \
  tar -C /data -czf /backup/bgh-data-$(date +%F).tar.gz .
```

For a fully consistent copy stop `bgh` during the backup, or use a
filesystem snapshot (LVM/ZFS/btrfs) of the data directory taken right after
the dump.

Restore (into an empty database and data directory, with `bgh` stopped):

```sh
systemctl stop bgh                       # or: docker compose stop bgh
pg_restore --clean --if-exists --no-owner --dbname="$DATABASE_URL" bgh-2026-01-01.dump
tar -C /var/lib/bgh -xzf bgh-data-2026-01-01.tar.gz && chown -R bgh:bgh /var/lib/bgh
systemctl start bgh                      # applies any newer migrations

# Docker Compose
docker compose stop bgh
docker compose exec -T postgres pg_restore -U bgh --clean --if-exists --no-owner -d bgh < bgh-2026-01-01.dump
docker run --rm -v bgh_bghdata:/data -v "$PWD":/backup debian:trixie-slim \
  sh -c 'tar -C /data -xzf /backup/bgh-data-2026-01-01.tar.gz && chown -R 10001:10001 /data'
docker compose start bgh
```

After restoring onto a new host keep the same `BGH_BASE_URL` (or update it:
URLs are generated on the fly, nothing stores the old one).

## Upgrades and migrations

* `bgh serve` applies pending migrations before it starts listening;
  `bgh migrate` applies them and exits (use it in a deploy step or init
  container). Migrations take a PostgreSQL advisory lock, so concurrent
  starts are safe.
* Migrations are **forward-only**: back up before upgrading; to roll back,
  restore the backup taken before the upgrade together with the old binary.
* Upgrade procedure: back up → replace the binary / `docker compose pull &&
  docker compose up -d` (or `--build`) → watch the logs (`journalctl -u bgh`
  / `docker compose logs -f bgh`) → check `/healthz`.
* Running several `bgh` processes against one database is supported for
  the database and Redis, but they must all see the same `BGH_DATA_DIR`
  (shared filesystem); a single node is the recommended setup today.

## Operations

* **Health:** `GET /healthz` → `200 {"status":"ok","database":true,"redis":true}`
  or `503` with `"degraded"`. `bgh healthcheck` probes it locally (exit
  status 0/1) without opening database connections itself.
* **Logs:** structured lines on stderr, one per request with a
  `request_id` (also returned as `X-Request-Id`); tune with `RUST_LOG`.
* **Shutdown:** SIGTERM/SIGINT stop accepting connections, finish in-flight
  requests, stop job workers, then exit.
* **Background jobs** live in the `jobs` table; failed jobs are retried
  with exponential backoff and kept with `failed_at` after the last attempt.
* **Resources:** the process is mostly I/O bound; git operations run as
  child processes. Size `BGH_DB_MAX_CONNECTIONS` below PostgreSQL's
  `max_connections` divided by the number of `bgh` processes.
