# syntax=docker/dockerfile:1.7
#
# Better GitHub: single-binary image (web client embedded in `bgh`).
#
#   docker build -t bgh .
#   docker run -p 3000:3000 -v bgh-data:/data \
#     -e DATABASE_URL=postgres://... -e REDIS_URL=redis://... bgh
#
# Stages: web (vite build) -> chef/planner/builder (cached Rust deps via
# cargo-chef) -> runtime (debian slim + git + git-lfs, non-root).
# See docs/SELF_HOSTING.md.

ARG RUST_VERSION=1.97
# cargo-chef image tag prefix: "latest" or a release such as "0.1.78".
ARG CARGO_CHEF_VERSION=latest
ARG NODE_VERSION=22
ARG DEBIAN_RELEASE=trixie

# --- web client ---------------------------------------------------------------
FROM node:${NODE_VERSION}-${DEBIAN_RELEASE}-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm \
    npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# --- rust toolchain + cargo-chef ----------------------------------------------
# The official rust slim image with a prebuilt cargo-chef, so cold builds
# don't compile it (#85).
FROM lukemathwalker/cargo-chef:${CARGO_CHEF_VERSION}-rust-${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
# OpenSSL headers: webauthn-rs (security keys / passkeys) links libssl.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- build ----------------------------------------------------------------------
FROM chef AS builder
# `embed-web` bakes web/dist into the binary. Set to "" to build a binary that
# serves the client from BGH_WEB_DIR instead (then copy web/dist yourself).
ARG CARGO_FEATURES=embed-web
# Parallel rustc jobs; lower it on small builders to cap memory.
ARG CARGO_BUILD_JOBS=
COPY --from=planner /src/recipe.json recipe.json
# Dependencies only: this layer is reused until Cargo.toml/Cargo.lock change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    if [ -n "$CARGO_BUILD_JOBS" ]; then export CARGO_BUILD_JOBS; else unset CARGO_BUILD_JOBS; fi; \
    cargo chef cook --release --locked --recipe-path recipe.json \
      -p bgh-server --features "$CARGO_FEATURES"
COPY . .
COPY --from=web /web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    if [ -n "$CARGO_BUILD_JOBS" ]; then export CARGO_BUILD_JOBS; else unset CARGO_BUILD_JOBS; fi; \
    cargo build --release --locked -p bgh-server --bin bgh --features "$CARGO_FEATURES" \
 && install -m 0755 target/release/bgh /usr/local/bin/bgh

# --- runtime --------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim AS runtime
# git: smart HTTP / SSH transport and write plumbing. tini: PID 1 that
# forwards signals and reaps orphaned git subprocesses. libssl: WebAuthn.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git git-lfs postgresql-client tini \
 && (apt-get install -y --no-install-recommends libssl3t64 \
     || apt-get install -y --no-install-recommends libssl3) \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 bgh \
 && useradd --system --uid 10001 --gid bgh --home-dir /data --shell /usr/sbin/nologin bgh \
 && mkdir -p /data \
 && chown bgh:bgh /data
COPY --from=builder /usr/local/bin/bgh /usr/local/bin/bgh

# Actions: this image has no docker, so with BGH_ACTIONS_EXECUTOR=auto the
# built-in runner takes no jobs (it never runs workflow code inside the
# server container). Use external runners (`bgh-runner`), or mount a docker
# socket and set BGH_ACTIONS_EXECUTOR=docker. See docs/SELF_HOSTING.md.
ENV BGH_LISTEN=0.0.0.0:3000 \
    BGH_DATA_DIR=/data \
    BGH_ACTIONS_EXECUTOR=auto \
    RUST_LOG=info,sqlx=warn

USER bgh
WORKDIR /data
VOLUME ["/data"]
# 3000: HTTP (API, web client, git smart HTTP). 2222: git over SSH.
EXPOSE 3000 2222
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD ["bgh", "healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "bgh"]
CMD ["serve"]
