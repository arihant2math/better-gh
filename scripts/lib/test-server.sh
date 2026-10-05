#!/usr/bin/env bash
# Shared helpers for scripts/gh-compat.sh and scripts/api-smoke.sh: start a
# throwaway bgh server (temp database, data dir and Redis prefix), optionally
# behind a local TLS proxy, and mint a user + personal access token.
#
# Source it; it defines functions and these globals:
#   WORK          temp directory (removed by ts_cleanup unless TS_KEEP=1)
#   BGH_BIN       server binary (built with cargo unless already set)
#   API_BASE      URL clients use (https://... when TLS is on)
#   HTTP_BASE     plain-HTTP URL of the server itself
#   TS_HOST       host[:port] of API_BASE (what GH_HOST must be)
#   TS_CA_FILE    CA certificate trusted for API_BASE (TLS mode)
#   TS_LOGIN / TS_PASSWORD / TS_TOKEN   test account
#   TS_STARTED    1 when we started the server (admin CLI available)
# shellcheck shell=bash

TS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TS_KEEP="${TS_KEEP:-0}"
TS_STARTED=0
TS_PIDS=()
TS_SERVER_ENV=()
TS_DB_NAME=""
TS_ADMIN_DB_URL=""
WORK=""

ts_log() { printf '\033[2m[%s]\033[0m %s\n' "${TS_TAG:-bgh}" "$*" >&2; }
ts_die() { printf '[%s] error: %s\n' "${TS_TAG:-bgh}" "$*" >&2; exit 1; }

ts_need() {
  local c
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || ts_die "required command not found: $c"
  done
}

ts_free_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'
}

# Can we bind 127.0.0.1:$1?
ts_port_bindable() {
  python3 - "$1" <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
EOF
}

ts_mkwork() {
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/bgh-${TS_TAG:-test}.XXXXXX")"
}

ts_cleanup() {
  local pid
  for pid in "${TS_PIDS[@]}"; do
    kill -TERM "$pid" 2>/dev/null || true
  done
  for pid in "${TS_PIDS[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
  TS_PIDS=()
  if [[ -n $TS_DB_NAME && $TS_KEEP != 1 ]]; then
    psql -q "$TS_ADMIN_DB_URL" -c "DROP DATABASE IF EXISTS \"$TS_DB_NAME\" WITH (FORCE)" >/dev/null 2>&1 || true
  fi
  if [[ -n $WORK && -d $WORK ]]; then
    if [[ $TS_KEEP == 1 ]]; then
      ts_log "kept $WORK${TS_DB_NAME:+ and database $TS_DB_NAME}"
    else
      rm -rf "$WORK"
    fi
  fi
}

# Build (or locate) the bgh binary.
ts_build() {
  if [[ -n ${BGH_BIN:-} ]]; then
    [[ -x $BGH_BIN ]] || ts_die "BGH_BIN=$BGH_BIN is not executable"
    return
  fi
  ts_log "building bgh (cargo build -p bgh-server)"
  (cd "$TS_ROOT" && cargo build --quiet -p bgh-server --bin bgh) || ts_die "cargo build failed"
  local target
  target="$(cd "$TS_ROOT" && cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
  BGH_BIN="$target/debug/bgh"
}

# Self-signed CA + localhost leaf certificate in $WORK/tls.
ts_make_certs() {
  local d="$WORK/tls"
  mkdir -p "$d"
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 2 \
    -subj "/CN=bgh test CA" -keyout "$d/ca.key" -out "$d/ca.pem" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
    >/dev/null 2>&1 || ts_die "openssl: creating CA failed"
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -subj "/CN=localhost" -keyout "$d/key.pem" -out "$d/leaf.csr" >/dev/null 2>&1 ||
    ts_die "openssl: creating CSR failed"
  printf '%s\n' "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" \
    "extendedKeyUsage=serverAuth" "subjectAltName=DNS:localhost,IP:127.0.0.1" >"$d/leaf.ext"
  openssl x509 -req -in "$d/leaf.csr" -CA "$d/ca.pem" -CAkey "$d/ca.key" -CAcreateserial \
    -days 2 -extfile "$d/leaf.ext" -out "$d/cert.pem" >/dev/null 2>&1 ||
    ts_die "openssl: signing certificate failed"
  TS_CA_FILE="$d/ca.pem"
}

# ts_start_tls_proxy UPSTREAM_HOST:PORT HTTPS_PORT
ts_start_tls_proxy() {
  local upstream="$1" port="$2" ready="$WORK/tls/ready"
  python3 "$TS_ROOT/scripts/lib/tls-proxy.py" --listen "127.0.0.1:$port" --upstream "$upstream" \
    --cert "$WORK/tls/cert.pem" --key "$WORK/tls/key.pem" --ready-file "$ready" \
    2>>"$WORK/tls-proxy.log" &
  TS_PIDS+=($!)
  local i
  for i in $(seq 1 50); do
    [[ -f $ready ]] && return 0
    sleep 0.1
  done
  ts_die "TLS proxy did not start (see $WORK/tls-proxy.log)"
}

ts_wait_healthy() {
  local url="$1" i
  for i in $(seq 1 120); do
    if curl -fsS --max-time 2 "$url/healthz" >/dev/null 2>&1; then
      return 0
    fi
    if [[ -n ${TS_SERVER_PID:-} ]] && ! kill -0 "$TS_SERVER_PID" 2>/dev/null; then
      tail -n 30 "$WORK/server.log" >&2
      ts_die "bgh exited during startup"
    fi
    [[ $i == 1 ]] || sleep 0.5
  done
  ts_die "server at $url not healthy after 60s"
}

# ts_start_server TLS(0|1) [HTTPS_PORT|auto]
# Starts bgh on a fresh database. Honors DATABASE_URL (server/credentials
# used to create the database), REDIS_URL and BGH_BIN.
ts_start_server() {
  local tls="$1" https_port="${2:-auto}"
  ts_need psql curl python3
  [[ $tls == 1 ]] && ts_need openssl
  ts_build

  local base_url="${DATABASE_URL:-postgres://postgres:postgres@localhost/bgh}"
  TS_DB_NAME="bgh_compat_$$_$RANDOM"
  TS_ADMIN_DB_URL="$(python3 - "$base_url" <<'EOF'
import sys, urllib.parse as u
p = u.urlsplit(sys.argv[1])
print(u.urlunsplit(p._replace(path="/postgres")))
EOF
)"
  local db_url
  db_url="$(python3 - "$base_url" "$TS_DB_NAME" <<'EOF'
import sys, urllib.parse as u
p = u.urlsplit(sys.argv[1])
print(u.urlunsplit(p._replace(path="/" + sys.argv[2])))
EOF
)"
  psql -q "$TS_ADMIN_DB_URL" -c "CREATE DATABASE \"$TS_DB_NAME\"" >/dev/null ||
    ts_die "could not create database $TS_DB_NAME (is postgres running? ./scripts/dev-setup.sh)"

  local http_port
  http_port="$(ts_free_port)"
  HTTP_BASE="http://127.0.0.1:$http_port"
  if [[ $tls == 1 ]]; then
    if [[ $https_port == auto ]]; then
      # Port 443 keeps GH_HOST port-less: gh drops ports when it matches git
      # remote URLs against GH_HOST, which breaks e.g. `gh pr checkout`.
      if [[ $(id -u) == 0 ]] && ts_port_bindable 443; then
        https_port=443
      else
        https_port="$(ts_free_port)"
      fi
    fi
    if [[ $https_port == 443 ]]; then TS_HOST="localhost"; else TS_HOST="localhost:$https_port"; fi
    API_BASE="https://$TS_HOST"
    ts_make_certs
  else
    TS_HOST="127.0.0.1:$http_port"
    API_BASE="$HTTP_BASE"
  fi

  mkdir -p "$WORK/data"
  # Concrete values (ts_admin reuses them after this function returns).
  TS_SERVER_ENV=(
    "DATABASE_URL=$db_url"
    "REDIS_URL=${REDIS_URL:-redis://127.0.0.1/}"
    "BGH_REDIS_PREFIX=bgh-compat:$$:$RANDOM:"
    "BGH_LISTEN=127.0.0.1:$http_port"
    "BGH_BASE_URL=$API_BASE"
    "BGH_DATA_DIR=$WORK/data"
    "BGH_WEB_DIR=$WORK/no-web"
    "BGH_JOB_WORKERS=2"
    "BGH_SSH_ENABLED=false"
    "RUST_LOG=${BGH_COMPAT_LOG:-info,sqlx=warn}"
  )
  env "${TS_SERVER_ENV[@]}" "$BGH_BIN" migrate >"$WORK/server.log" 2>&1 ||
    { tail -n 20 "$WORK/server.log" >&2; ts_die "bgh migrate failed"; }
  env "${TS_SERVER_ENV[@]}" "$BGH_BIN" serve >>"$WORK/server.log" 2>&1 &
  TS_SERVER_PID=$!
  TS_PIDS+=("$TS_SERVER_PID")
  TS_STARTED=1
  ts_wait_healthy "$HTTP_BASE"
  if [[ $tls == 1 ]]; then
    ts_start_tls_proxy "127.0.0.1:$http_port" "$https_port"
    curl -fsS --cacert "$TS_CA_FILE" "$API_BASE/healthz" >/dev/null ||
      ts_die "TLS proxy at $API_BASE not reachable"
  fi
  ts_log "server: $API_BASE (db $TS_DB_NAME, logs $WORK/server.log)"
}

# Use an already running server. ts_use_server URL TLS(0|1) [CACERT]
ts_use_server() {
  local url="${1%/}" tls="$2" cacert="${3:-}"
  ts_need curl python3
  HTTP_BASE="$url"
  if [[ $url == https://* ]]; then
    API_BASE="$url"
    TS_HOST="${url#https://}"
    TS_CA_FILE="$cacert"
  elif [[ $tls == 1 ]]; then
    ts_need openssl
    ts_make_certs
    local https_port
    https_port="$(ts_free_port)"
    TS_HOST="localhost:$https_port"
    API_BASE="https://$TS_HOST"
    ts_start_tls_proxy "${url#http://}" "$https_port"
    ts_log "warning: $url generates links for its own BGH_BASE_URL, not $API_BASE"
  else
    API_BASE="$url"
    TS_HOST="${url#http://}"
  fi
  ts_wait_healthy "$HTTP_BASE"
}

# curl against API_BASE, trusting the test CA. Extra args are passed through.
ts_curl() {
  local args=(-sS --max-time 30)
  [[ -n ${TS_CA_FILE:-} ]] && args+=(--cacert "$TS_CA_FILE")
  curl "${args[@]}" "$@"
}

# Run `bgh admin ...` against the server we started.
ts_admin() {
  [[ $TS_STARTED == 1 ]] || ts_die "bgh admin needs a server started by this script"
  env "${TS_SERVER_ENV[@]}" "$BGH_BIN" admin "$@"
}

# Create the test account and a PAT. Sets TS_LOGIN, TS_PASSWORD, TS_TOKEN.
# Servers we started: `bgh admin create-user` + `bgh admin create-token`.
# Running servers: sign-up, then POST /_bgh/tokens with the session cookie.
TS_SCOPES="repo,admin:org,workflow,gist,user,delete_repo,admin:public_key,admin:repo_hook,notifications,project"
ts_create_account() {
  TS_LOGIN="${1:-compat-$RANDOM}"
  TS_PASSWORD="compat-$RANDOM-$RANDOM-password"
  if [[ $TS_STARTED == 1 ]]; then
    ts_admin create-user --login "$TS_LOGIN" --email "$TS_LOGIN@example.com" \
      --password "$TS_PASSWORD" --site-admin >/dev/null || ts_die "bgh admin create-user failed"
    TS_TOKEN="$(ts_admin create-token --user "$TS_LOGIN" --scopes "$TS_SCOPES,site_admin" \
      --name compat 2>/dev/null)" || ts_die "bgh admin create-token failed"
  else
    local jar="$WORK/cookies.txt" body resp scopes_json
    body="$(printf '{"login":"%s","email":"%s@example.com","password":"%s"}' \
      "$TS_LOGIN" "$TS_LOGIN" "$TS_PASSWORD")"
    ts_curl -f -c "$jar" -H 'content-type: application/json' -d "$body" \
      "$API_BASE/_bgh/signup" >/dev/null || ts_die "sign-up (POST /_bgh/signup) failed; pass --token instead"
    scopes_json="[\"${TS_SCOPES//,/\",\"}\"]"
    resp="$(ts_curl -f -b "$jar" -H 'content-type: application/json' \
      -d "{\"name\":\"compat\",\"scopes\":$scopes_json}" "$API_BASE/_bgh/tokens")" ||
      ts_die "creating a token (POST /_bgh/tokens) failed"
    TS_TOKEN="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])' <<<"$resp")"
  fi
  [[ -n $TS_TOKEN ]] || ts_die "no token created"
  ts_log "account: $TS_LOGIN (token …${TS_TOKEN: -4})"
}

# Login of the token's owner (GET /user).
ts_login_for_token() {
  ts_curl -f -H "authorization: token $TS_TOKEN" "$API_BASE/api/v3/user" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["login"])'
}
