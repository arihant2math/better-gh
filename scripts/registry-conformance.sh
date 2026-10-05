#!/usr/bin/env bash
# Container registry (OCI distribution) checks against a throwaway bgh:
#
# 1. the official OCI distribution-spec conformance suite
#    (github.com/opencontainers/distribution-spec/conformance), built with
#    `go test -c` unless OCI_CONFORMANCE_BIN points at a built binary;
# 2. a real `docker` client (login, build, push, pull, multi-arch-free
#    smoke) when a docker daemon is reachable.
#
# Usage: scripts/registry-conformance.sh [--keep] [--skip-docker] [--skip-conformance]
# Results (JUnit/HTML) of the conformance suite go to $OCI_RESULTS_DIR
# (default: <work dir>/results, kept with --keep).
# shellcheck disable=SC2317
set -uo pipefail

TS_TAG=registry
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-server.sh"

SKIP_DOCKER=0 SKIP_CONFORMANCE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep) TS_KEEP=1; shift ;;
    --skip-docker) SKIP_DOCKER=1; shift ;;
    --skip-conformance) SKIP_CONFORMANCE=1; shift ;;
    -h|--help) awk 'NR==1{next} !/^#/{exit} /shellcheck/{next} {sub(/^# ?/,""); print}' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

ts_mkwork
trap ts_cleanup EXIT
trap 'exit 130' INT TERM
ts_start_server 0
TS_SCOPES="repo,write:packages,delete:packages,read:packages"
ts_create_account "reg-$RANDOM"
LOGIN="$(tr '[:upper:]' '[:lower:]' <<<"$TS_LOGIN")"
FAILED=0

if [[ $SKIP_CONFORMANCE == 0 ]]; then
  BIN="${OCI_CONFORMANCE_BIN:-}"
  if [[ -z $BIN ]]; then
    ts_need go git
    ts_log "building the OCI conformance suite"
    git clone -q --depth 1 https://github.com/opencontainers/distribution-spec.git "$WORK/dist-spec" ||
      ts_die "cloning distribution-spec failed"
    (cd "$WORK/dist-spec/conformance" && go test -c -o "$WORK/conformance.test") ||
      ts_die "building the conformance suite failed"
    BIN="$WORK/conformance.test"
  fi
  export OCI_ROOT_URL="$HTTP_BASE"
  export OCI_NAMESPACE="$LOGIN/conformance-a"
  export OCI_CROSSMOUNT_NAMESPACE="$LOGIN/conformance-b"
  export OCI_REGISTRY="${HTTP_BASE#http://}"
  export OCI_TLS=disabled
  export OCI_REPO1="$LOGIN/conformance-a"
  export OCI_REPO2="$LOGIN/conformance-b"
  export OCI_USERNAME="$TS_LOGIN" OCI_PASSWORD="$TS_TOKEN"
  export OCI_VERSION="${OCI_VERSION:-1.1}"
  export OCI_TEST_PULL=1 OCI_TEST_PUSH=1 OCI_TEST_CONTENT_DISCOVERY=1 OCI_TEST_CONTENT_MANAGEMENT=1
  export OCI_RESULTS_DIR="${OCI_RESULTS_DIR:-$WORK/results}"
  mkdir -p "$OCI_RESULTS_DIR"
  ts_log "running the OCI conformance suite against $OCI_REGISTRY"
  if (cd "$OCI_RESULTS_DIR" && "$BIN" -test.v) >"$WORK/conformance.log" 2>&1; then
    ts_log "conformance: PASS"
  else
    FAILED=1
    ts_log "conformance: FAIL (log: $WORK/conformance.log)"
  fi
  grep -E "^(--- FAIL|FAIL|ok|PASS)" "$WORK/conformance.log" | tail -20 >&2 || true
  grep -E "Ran [0-9]+ of|Passed|Failed" "$WORK/conformance.log" | tail -5 >&2 || true
fi

if [[ $SKIP_DOCKER == 0 ]] && command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  REG="${HTTP_BASE#http://}"
  IMAGE="$REG/$LOGIN/docker-smoke"
  ctx="$WORK/docker-ctx"
  mkdir -p "$ctx"
  printf 'hello from bgh %s\n' "$RANDOM" >"$ctx/hello.txt"
  printf 'FROM scratch\nCOPY hello.txt /hello.txt\nLABEL org.opencontainers.image.source=%s/%s/none\n' \
    "$HTTP_BASE" "$LOGIN" >"$ctx/Dockerfile"
  # Logs the command without its last argument for `login` (a secret).
  step() {
    local what="$*"
    [[ $1 == login ]] && what="login $2"
    if "$@" >>"$WORK/docker.log" 2>&1; then ts_log "docker: ok   $what"; else ts_log "docker: FAIL $what (log: $WORK/docker.log)"; FAILED=1; fi
  }
  # Passwords are refused, tokens accepted.
  if docker login "$REG" -u "$TS_LOGIN" --password-stdin <<<"$TS_PASSWORD" >>"$WORK/docker.log" 2>&1; then
    ts_log "docker: FAIL login with a password was accepted"; FAILED=1
  else
    ts_log "docker: ok   password login refused"
  fi
  login() { docker login "$REG" -u "$1" --password-stdin <<<"$2"; }
  step login "$TS_LOGIN" "$TS_TOKEN"
  step docker build -q -t "$IMAGE:v1" "$ctx"
  step docker push "$IMAGE:v1"
  step docker rmi "$IMAGE:v1"
  step docker pull "$IMAGE:v1"
  step docker logout "$REG"
  # Anonymous pull of the (private) package is refused.
  docker rmi "$IMAGE:v1" >/dev/null 2>&1 || true
  anon_out="$(docker pull "$IMAGE:v1" 2>&1)"
  anon_rc=$?
  printf '%s\n' "$anon_out" >>"$WORK/docker.log"
  if [[ $anon_rc == 0 ]] || ! grep -qiE "unauthorized|denied|authentication" <<<"$anon_out"; then
    ts_log "docker: FAIL anonymous pull of a private image"; FAILED=1
  else
    ts_log "docker: ok   anonymous pull of a private image refused"
  fi

  # An Actions job token (GITHUB_TOKEN: `actions:repo:<id>` scope) pushes to
  # its repository's namespace; the package gets linked to the repository.
  repo_id="$(curl -sf -H "authorization: token $TS_TOKEN" -H 'content-type: application/json' \
    -d '{"name":"ci-app","private":true}' "$HTTP_BASE/api/v3/user/repos" | jq -r .id)"
  # Minted like bgh-actions does when a job is claimed (the CLI only
  # issues classic scopes).
  job_token="bghp_$(python3 -c 'import secrets,string; print("".join(secrets.choice(string.ascii_letters+string.digits) for _ in range(40)))')"
  db_url=""
  for kv in "${TS_SERVER_ENV[@]}"; do [[ $kv == DATABASE_URL=* ]] && db_url="${kv#DATABASE_URL=}"; done
  psql -q "$db_url" -c "INSERT INTO access_tokens (user_id, name, token_hash, token_last_eight, scopes, kind, expires_at)
    SELECT id, 'GITHUB_TOKEN (e2e)', encode(sha256('$job_token'::bytea), 'hex'), right('$job_token', 8),
           ARRAY['repo', 'workflow', 'actions:repo:$repo_id'], 'app', now() + interval '1 hour'
      FROM users WHERE login = '$TS_LOGIN'" >/dev/null || ts_log "minting a job token failed"
  step login "$TS_LOGIN" "$TS_TOKEN"
  step docker pull "$IMAGE:v1"
  step login x-access-token "$job_token"
  step docker tag "$IMAGE:v1" "$REG/$LOGIN/ci-app:sha-1"
  step docker push "$REG/$LOGIN/ci-app:sha-1"
  linked="$(curl -sf -H "authorization: token $TS_TOKEN" "$HTTP_BASE/_bgh/repos/$LOGIN/ci-app/packages" |
    jq -r '.packages[0].name // empty')"
  if [[ $linked == ci-app ]]; then
    ts_log "docker: ok   GITHUB_TOKEN push linked the package to the repository"
  else
    ts_log "docker: FAIL GITHUB_TOKEN push did not link the package"; FAILED=1
  fi
  docker rmi "$REG/$LOGIN/ci-app:sha-1" >/dev/null 2>&1 || true
  docker logout "$REG" >/dev/null 2>&1 || true
else
  ts_log "docker: SKIP (no reachable docker daemon)"
fi

exit "$FAILED"
