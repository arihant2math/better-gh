#!/usr/bin/env bash
# F5 end-to-end check against the REAL backend: starts a throwaway bgh server
# (fresh database, serving web/dist), creates users + tokens, seeds many
# issues / PRs / comments / notifications over the REST API
# (scripts/seed-inbox.mjs), then runs the Playwright checks
# (scripts/inbox-search-e2e.mjs): inbox triage, live arrival, watch dialog,
# palette + search page, dashboard feed. Screenshots go to OUT_DIR.
#
#   cargo build -p bgh-server && (cd web && npm run build)
#   web/scripts/inbox-e2e.sh [OUT_DIR] [--keep] [--scale N]
#
# Env: DATABASE_URL (server/credentials used to create the DB), REDIS_URL,
# BGH_BIN (default target/debug/bgh), PLAYWRIGHT_BROWSERS_PATH.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="" KEEP=0 SCALE=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep) KEEP=1; shift ;;
    --scale) SCALE="$2"; shift 2 ;;
    *) OUT="$1"; shift ;;
  esac
done
OUT="${OUT:-$ROOT/web/e2e-shots}"
mkdir -p "$OUT"
BIN="${BGH_BIN:-$ROOT/target/debug/bgh}"
[[ -x $BIN ]] || { echo "missing $BIN (cargo build -p bgh-server)" >&2; exit 1; }
[[ -f $ROOT/web/dist/index.html ]] || { echo "missing web/dist (cd web && npm run build)" >&2; exit 1; }

BASE_DB="${DATABASE_URL:-postgres://postgres:postgres@localhost/bgh}"
DB_NAME="bgh_f5_$$"
ADMIN_DB="${BASE_DB%/*}/postgres"
DB_URL="${BASE_DB%/*}/$DB_NAME"
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
WORK="$(mktemp -d)"
BASE="http://127.0.0.1:$PORT"
psql -q "$ADMIN_DB" -c "CREATE DATABASE \"$DB_NAME\"" >/dev/null

ENVV=(
  "DATABASE_URL=$DB_URL"
  "REDIS_URL=${REDIS_URL:-redis://127.0.0.1/}"
  "BGH_REDIS_PREFIX=bgh-f5:$$:"
  "BGH_LISTEN=127.0.0.1:$PORT"
  "BGH_BASE_URL=$BASE"
  "BGH_DATA_DIR=$WORK/data"
  "BGH_WEB_DIR=$ROOT/web/dist"
  "BGH_JOB_WORKERS=2"
  "BGH_SSH_ENABLED=false"
  "RUST_LOG=${BGH_LOG:-warn}"
)
cleanup() {
  [[ -n ${PID:-} ]] && kill "$PID" 2>/dev/null || true
  wait 2>/dev/null || true
  if [[ $KEEP == 0 ]]; then
    psql -q "$ADMIN_DB" -c "DROP DATABASE IF EXISTS \"$DB_NAME\"" >/dev/null 2>&1 || true
    rm -rf "$WORK"
  else
    echo "kept: db $DB_NAME, data $WORK"
  fi
}
trap cleanup EXIT

mkdir -p "$WORK/data"
env "${ENVV[@]}" "$BIN" migrate >"$WORK/server.log" 2>&1
env "${ENVV[@]}" "$BIN" serve >>"$WORK/server.log" 2>&1 &
PID=$!
for _ in $(seq 1 100); do curl -fsS "$BASE/healthz" >/dev/null 2>&1 && break; sleep 0.2; done
curl -fsS "$BASE/healthz" >/dev/null || { tail -20 "$WORK/server.log"; exit 1; }

SCOPES="repo,admin:org,user,notifications,delete_repo"
TOKENS="$WORK/tokens.json"
echo '{' >"$TOKENS"
first=1
for u in ada grace linus margaret alan barbara; do
  admin_flag="" scopes="$SCOPES"
  [[ $u == ada ]] && admin_flag="--site-admin" scopes="$SCOPES,site_admin"
  # shellcheck disable=SC2086 # empty flag must vanish
  env "${ENVV[@]}" "$BIN" admin create-user --login "$u" --email "$u@example.com" --password "password-$u-123" $admin_flag >/dev/null
  tok="$(env "${ENVV[@]}" "$BIN" admin create-token --user "$u" --scopes "$scopes" --name e2e)"
  [[ $first == 1 ]] || echo ',' >>"$TOKENS"
  printf '"%s": "%s"' "$u" "$tok" >>"$TOKENS"
  first=0
done
echo '}' >>"$TOKENS"

echo "server $BASE (db $DB_NAME, log $WORK/server.log)"
node "$ROOT/web/scripts/seed-inbox.mjs" "$BASE" "$TOKENS" "$SCALE"
PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers}" \
  node "$ROOT/web/scripts/inbox-search-e2e.mjs" "$BASE" "$TOKENS" "$OUT"
