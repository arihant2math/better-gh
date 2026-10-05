Integration: ready
P61 observability: Prometheus `/metrics` (bearer token or private listener, off by default), `BGH_LOG_FORMAT=json`, request span with user/token/auth method/client IP, optional OTLP traces (`--features otlp`).

# P61 Observability — status

Branch `bgh/p61-observability`. Scope: `docs/PHASE4_PLAN.md` §P61 (no §5
quick fixes assigned). Evidence: `docs/AUDIT.md` "No Prometheus /metrics
endpoint or OpenTelemetry export" and "Logs are plain text only".
Migrations: none (7300–7399 unused). No web UI changes.

## What landed

* `bgh_core::observability`: metric name constants, `describe()`, and
  helpers on the `metrics` facade (`http_request`, `ratelimit_rejected`,
  `job_finished`, `redis_error`/`redis_result`, `git_op` → `GitTimer`,
  `ws_connected` → guard, `webhook_delivery`, `record_caller`). Labels are
  `&'static str`: methods/status codes from static tables, route
  templates and job kinds interned once (bounded at 4096, then `other`),
  so recording a request does not allocate.
* `bgh_server::telemetry`:
  * `install()` — global Prometheus recorder (idempotent, called from
    `app()`), buckets 5 ms…300 s for every `*_seconds` histogram.
  * `http_metrics` (outermost layer) + `route_label` (`route_layer` on the
    API router and the root router; the matched template travels outwards
    in the response extensions). Unmatched paths (SPA, unknown API paths)
    → `route="unmatched"`.
  * `GET /metrics` on the main listener: 404 unless `BGH_METRICS_TOKEN`,
    then bearer-checked (constant time) with 401 + `WWW-Authenticate:
    Bearer`. `BGH_METRICS_LISTEN` serves `metrics_router()` on a separate
    listener (open unless the token is also set), spawned in `main.rs`.
  * Scrape-time gauges: job queue depth/failed by kind (stale kinds reset
    to 0), event consumer lag + oldest pending age (P9's
    `outbox::consumer_lag`), DB pool in-use/idle/max, Actions queued /
    in progress (`actions_jobs_active_idx`).
  * `init_logging()`: `BGH_LOG_FORMAT=json|pretty`, `json_layer()` (flat
    event fields + current span under `span`), OTLP/HTTP export behind
    the `otlp` cargo feature (`BGH_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`;
    without the feature the variable logs a warning).
* Request span `http` gains `client_ip` (`auth::client_ip`, trusted-proxy
  aware) and empty `user_id`, `token_id`, `auth_method`, recorded by
  `auth::authenticate` on success (covers extractors, rate limiter, git
  basic auth, WebSocket).
* Instrumentation: jobs (`run_one`: ok/retry/failed + duration), rate
  limiter rejections and Redis failures (`ratelimit`, `throttle`,
  `session` cache, `sync_publish`), git (`upload-pack` HTTP + SSH,
  `receive-pack`, `archive` (uncached runs), `merge-tree`), sync WebSocket
  gauge, webhook deliveries (success/failure/error).
* Docs: `docs/SELF_HOSTING.md` "Monitoring" (metric table, scrape config,
  alert starting points, JSON log example, traces) + env var rows;
  `docs/ARCHITECTURE.md` configuration + HTTP surface.

## Shared-code changes (additive)

* Workspace `Cargo.toml`: `tracing-subscriber` `json` feature; new deps
  `metrics 0.24`, `metrics-exporter-prometheus 0.17` (no default
  features: no HTTP server), optional `opentelemetry*` 0.31 /
  `tracing-opentelemetry` 0.32 (reuse reqwest 0.12; Cargo.lock change is
  additions only).
* `bgh-core`: new `observability` module; `Config.metrics_token`,
  `Config.metrics_listen`; one-line hooks in `auth.rs`, `jobs.rs`,
  `ratelimit.rs`, `sync.rs`.
* `bgh-git` (`smart_http.rs`, `archive.rs`, `merge.rs`), `bgh-sync`
  (`ws.rs`), `bgh-notify` (`webhooks/deliver.rs`): timer/counter calls only.

## Tests

`crates/bgh-server/tests/it/observability.rs`: not public by default
(404 JSON, not the SPA), token required (401 without/wrong/PAT, 200 with
bearer, content type), metric families after a smoke run (API requests
by template incl. 404 and `unmatched`, no raw paths, histogram buckets,
real `git clone` → upload-pack metrics, pool, Actions, consumer lag),
job queue depth + job outcome by kind, rate-limit rejections, the
dedicated listener router, JSON log lines parse and carry `request_id`,
`user_id`, `token_id`, `auth_method`, `client_ip`. Unit tests for label
interning and bearer matching. Also verified manually: `bgh serve` with
`BGH_LOG_FORMAT=json BGH_METRICS_LISTEN=…`, and `cargo clippy -p
bgh-server --features otlp`.

## Known gaps / TODOs

* HTTP latency is time to response headers; streamed bodies (clones,
  archives) are covered by the git duration histograms instead.
* Counters are per process (sum across processes); scrape gauges are
  global (DB-derived) and identical on every process.
* The Docker image is built without `otlp`; enable with
  `--features bgh-server/otlp` if needed.
* No admin UI page for metrics (not in scope; the admin health page is
  unchanged).
