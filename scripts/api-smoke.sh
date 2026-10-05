#!/usr/bin/env bash
# REST API smoke test: core GitHub response shapes, status codes and headers
# checked with curl + jq. Complements scripts/gh-compat.sh (real gh CLI) and
# the per-crate integration tests.
#
#   scripts/api-smoke.sh                       start a throwaway server (temp DB + data dir)
#   scripts/api-smoke.sh --url http://localhost:3000 --token bghp_...
#   scripts/api-smoke.sh --json smoke.json     also write machine-readable results
#
# Options: --url URL, --token TOKEN, --cacert FILE, --json FILE,
#          --filter REGEX, --strict (exit 1 on failures), --keep, -v/--verbose.
# Environment: DATABASE_URL / REDIS_URL / BGH_BIN as for gh-compat.sh.
# SC2034: fixture flags (REPO_OK, ...) are read via ${!var}.
# SC2016: jq programs are single-quoted on purpose.
# shellcheck disable=SC2034,SC2016
set -uo pipefail

TS_TAG=api-smoke
# shellcheck source=scripts/lib/test-server.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-server.sh"
# shellcheck source=scripts/lib/report.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/report.sh"

URL="" TOKEN="" CACERT="" JSON_OUT="" FILTER="" STRICT=0 VERBOSE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --cacert) CACERT="$2"; shift 2 ;;
    --json) JSON_OUT="$2"; shift 2 ;;
    --filter) FILTER="$2"; shift 2 ;;
    --strict) STRICT=1; shift ;;
    --keep) TS_KEEP=1; shift ;;
    -v|--verbose) VERBOSE=1; shift ;;
    -h|--help) awk 'NR==1{next} !/^#/{exit} /shellcheck|^# SC[0-9]/{next} {sub(/^# ?/,""); print}' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
[[ -n $JSON_OUT && $JSON_OUT != /* ]] && JSON_OUT="$PWD/$JSON_OUT"
ts_need curl jq python3

ts_mkwork
trap ts_cleanup EXIT
trap 'exit 130' INT TERM
if [[ -n $URL ]]; then
  ts_use_server "$URL" 0 "$CACERT"
else
  ts_start_server 0
fi
if [[ -n $TOKEN ]]; then
  TS_TOKEN="$TOKEN"
  TS_LOGIN="$(ts_login_for_token)" || ts_die "token rejected by GET /user"
else
  ts_create_account
fi
LOGIN="$TS_LOGIN"
# Fixture flags set by passing checks, read through --needs (indirect expansion).
REPO_OK="" ISSUE_OK=""
report_init
N=0

# check NAME [options] -- PATH
#   --status CODE        expected HTTP status (required)
#   --method M           default GET (POST when --data is given)
#   --data JSON          request body (sent as application/json)
#   --raw-data STRING    request body sent verbatim
#   --anon | --bad-auth  no credentials / an invalid token (default: the test PAT)
#   -H 'Name: value'     extra request header
#   --jq EXPR            must evaluate to true on the JSON body (repeatable)
#   --header 'name~re'   response header must exist and match the regex (repeatable)
#   --needs VAR          SKIP unless $VAR is non-empty
check() {
  local name="$1"; shift
  local status="" method="" data="" auth=token needs=() jqs=() hdrs=() extra=()
  while [[ $# -gt 0 && $1 != -- ]]; do
    case "$1" in
      --status) status="$2"; shift 2 ;;
      --method) method="$2"; shift 2 ;;
      --data) data="$2"; extra+=(-H "content-type: application/json"); shift 2 ;;
      --raw-data) data="$2"; shift 2 ;;
      --anon) auth=anon; shift ;;
      --bad-auth) auth=bad; shift ;;
      -H) extra+=(-H "$2"); shift 2 ;;
      --jq) jqs+=("$2"); shift 2 ;;
      --header) hdrs+=("$2"); shift 2 ;;
      --needs) needs+=("$2"); shift 2 ;;
      *) echo "check: bad option $1" >&2; return 2 ;;
    esac
  done
  local path="$2"
  [[ -n $FILTER ]] && ! [[ $name =~ $FILTER ]] && return 0
  [[ -z $method ]] && { [[ -n $data ]] && method=POST || method=GET; }
  local cmd="$method $path" v
  for v in "${needs[@]}"; do
    if [[ -z ${!v:-} ]]; then
      record api "$name" SKIP "" 0 "$cmd" "fixture unavailable: $v"
      return 0
    fi
  done
  N=$((N + 1))
  local body="$WORK/body.$N" headers="$WORK/headers.$N"
  local args=(-X "$method" -o "$body" -D "$headers" -w '%{http_code}')
  case $auth in
    token) args+=(-H "authorization: token $TS_TOKEN") ;;
    bad) args+=(-H "authorization: token bghp_0000000000000000000000000000000000000000") ;;
  esac
  args+=(-H "accept: application/vnd.github+json" "${extra[@]}")
  [[ -n $data ]] && args+=(--data-binary "$data")
  local start code ms detail=""
  start=$(now_ms)
  code="$(ts_curl "${args[@]}" "$API_BASE$path" 2>"$WORK/curl.$N")"
  ms=$(($(now_ms) - start))
  if [[ $code != "$status" ]]; then
    detail="HTTP $code (want $status)"
    local msg
    msg="$(jq -r '.message? // empty' "$body" 2>/dev/null | head -n 1)"
    [[ -z $msg ]] && msg="$(head -c 120 "$body" 2>/dev/null)"
    [[ -z $msg && $code == 000 ]] && msg="$(cat "$WORK/curl.$N")"
    [[ -n $msg ]] && detail+=": $msg"
  else
    local expr h hname hre hval
    for expr in "${jqs[@]}"; do
      if ! jq -e "$expr" "$body" >/dev/null 2>&1; then
        detail="jq: $expr is not true"
        break
      fi
    done
    if [[ -z $detail ]]; then
      for h in "${hdrs[@]}"; do
        hname="${h%%~*}" hre="${h#*~}"
        hval="$(grep -i "^$hname:" "$headers" | head -n 1 | cut -d: -f2- | tr -d '\r' | sed 's/^ //')"
        if [[ -z $hval ]] || ! [[ $hval =~ $hre ]]; then
          detail="header $hname: ${hval:-<missing>} !~ /$hre/"
          break
        fi
      done
    fi
  fi
  if [[ -z $detail ]]; then
    record api "$name" PASS "$code" "$ms" "$cmd" ""
    return 0
  fi
  record api "$name" FAIL "$code" "$ms" "$cmd" "$detail"
  if [[ $VERBOSE == 1 ]]; then
    { cat "$headers"; head -c 2000 "$body"; echo; } | sed 's/^/      | /'
  fi
  return 1
}

TS_RE='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'
echo "== API smoke test against $API_BASE as $LOGIN"

echo "-- infrastructure"
check "healthz" --anon --status 200 --jq '.status == "ok" and .database and .redis' -- /healthz
check "unknown API path is a JSON 404" --status 404 \
  --jq '.message == "Not Found" and (.documentation_url | type == "string")' -- /api/v3/definitely/not/here
check "unknown /_bgh path is a JSON 404" --anon --status 404 --jq '.message == "Not Found"' -- /_bgh/nope
check "CORS preflight" --anon --method OPTIONS --status 200 -H "origin: https://example.com" \
  -H "access-control-request-method: GET" --header 'access-control-allow-origin~.' -- /api/v3/user
check "request id header" --anon --status 200 --header 'x-request-id~^[0-9a-f-]{36}$' -- /healthz
check "API root" --status 200 --jq '.current_user_url | type == "string"' -- /api/v3/
check "rate limit" --status 200 --jq '.resources.core.limit > 0 and .rate.remaining >= 0' -- /api/v3/rate_limit
check "meta" --anon --status 200 --jq '.verifiable_password_authentication | type == "boolean"' -- /api/v3/meta

echo "-- authentication"
check "anonymous GET /user is 401" --anon --status 401 \
  --jq '.message == "Requires authentication"' -- /api/v3/user
check "bad token is 401 Bad credentials" --bad-auth --status 401 \
  --jq '.message == "Bad credentials"' -- /api/v3/user
check "GET /user (PAT)" --status 200 \
  --jq ".login == \"$LOGIN\" and (.id | type == \"number\") and (.node_id | type == \"string\")" \
  --jq '.login as $l | .type == "User" and (.url | endswith("/api/v3/users/" + $l)) and (.html_url | endswith("/" + $l))' \
  --jq "(.created_at | test(\"$TS_RE\"))" \
  --header 'x-oauth-scopes~repo' --header 'x-github-media-type~^github\.v3' --header 'etag~^W/"' \
  -- /api/v3/user
ETAG="$(grep -i '^etag:' "$WORK/headers.$N" | cut -d' ' -f2- | tr -d '\r')"
check "conditional GET (If-None-Match) is 304" --status 304 -H "if-none-match: $ETAG" -- /api/v3/user
check "GET /users/{login}" --anon --status 200 \
  --jq ".login == \"$LOGIN\" and (has(\"plan\") | not)" -- "/api/v3/users/$LOGIN"
check "GET /users/{missing} is 404" --anon --status 404 -- /api/v3/users/no-such-user-zz

echo "-- repositories"
check "malformed JSON is 400" --raw-data '{"name":' -H "content-type: application/json" \
  --method POST --status 400 --jq '.message == "Problems parsing JSON"' -- /api/v3/user/repos
check "POST /user/repos" --data '{"name":"smoke","description":"api smoke","auto_init":true}' --status 201 \
  --jq '.name == "smoke" and .full_name == "'"$LOGIN"'/smoke" and .private == false' \
  --jq '.owner.login == "'"$LOGIN"'" and .default_branch == "main" and .permissions.admin == true' \
  --jq '(.clone_url | endswith("/'"$LOGIN"'/smoke.git")) and (.issues_url | contains("{/number}"))' \
  --jq "(.created_at | test(\"$TS_RE\")) and (.node_id | type == \"string\")" \
  -- /api/v3/user/repos && REPO_OK=1
check "duplicate repo name is 422" --data '{"name":"smoke"}' --status 422 \
  --jq '.errors[0] | .resource == "Repository" and .field == "name" and .code == "custom"' \
  --jq '.message == "Repository creation failed."' -- /api/v3/user/repos
check "missing name is 422" --data '{}' --status 422 --jq '.errors | length > 0' -- /api/v3/user/repos
check "POST /user/repos (private)" --data '{"name":"smoke-private","private":true}' --status 201 \
  --jq '.private == true and .visibility == "private"' -- /api/v3/user/repos
check "pagination Link header" --status 200 --jq 'length == 1' \
  --header 'link~rel="next"' -- "/api/v3/user/repos?per_page=1"
check "GET /repos/{owner}/{repo}" --needs REPO_OK --status 200 \
  --jq '.full_name == "'"$LOGIN"'/smoke" and (.owner | has("avatar_url"))' -- "/api/v3/repos/$LOGIN/smoke"
check "private repo is 404 anonymously" --anon --status 404 -- "/api/v3/repos/$LOGIN/smoke-private"
check "GET /users/{login}/repos" --anon --status 200 \
  --jq 'map(.name) | index("smoke") != null and index("smoke-private") == null' -- "/api/v3/users/$LOGIN/repos"
check "PATCH /repos/{owner}/{repo}" --needs REPO_OK --method PATCH --data '{"description":"patched"}' \
  --status 200 --jq '.description == "patched"' -- "/api/v3/repos/$LOGIN/smoke"

echo "-- git & contents"
check "smart HTTP info/refs" --needs REPO_OK --status 200 \
  --header 'content-type~^application/x-git-upload-pack-advertisement' \
  -- "/$LOGIN/smoke.git/info/refs?service=git-upload-pack"
check "GET contents/README.md" --needs REPO_OK --status 200 \
  --jq '.type == "file" and .encoding == "base64" and .name == "README.md" and (.sha | length == 40)' \
  -- "/api/v3/repos/$LOGIN/smoke/contents/README.md"
check "GET branches" --needs REPO_OK --status 200 --jq 'map(.name) | index("main") != null' \
  -- "/api/v3/repos/$LOGIN/smoke/branches"
check "GET commits" --needs REPO_OK --status 200 --jq '.[0].sha | test("^[0-9a-f]{40}$")' \
  -- "/api/v3/repos/$LOGIN/smoke/commits"
check "GET git/ref/heads/main" --needs REPO_OK --status 200 \
  --jq '.ref == "refs/heads/main" and (.object.sha | length == 40)' \
  -- "/api/v3/repos/$LOGIN/smoke/git/ref/heads/main"

echo "-- issues & labels"
check "GET labels" --needs REPO_OK --status 200 --jq 'type == "array"' -- "/api/v3/repos/$LOGIN/smoke/labels"
check "POST labels" --needs REPO_OK --data '{"name":"smoke","color":"ededed"}' --status 201 \
  --jq '.name == "smoke" and .color == "ededed" and (.url | type == "string")' \
  -- "/api/v3/repos/$LOGIN/smoke/labels"
check "POST issues" --needs REPO_OK --data '{"title":"Smoke issue","body":"hello","labels":["smoke"]}' \
  --status 201 --jq '.number == 1 and .state == "open" and .user.login == "'"$LOGIN"'"' \
  --jq '(.html_url | endswith("/issues/1")) and (.pull_request | not)' \
  -- "/api/v3/repos/$LOGIN/smoke/issues" && ISSUE_OK=1
check "GET issues" --needs ISSUE_OK --status 200 --jq 'length >= 1 and .[0].title == "Smoke issue"' \
  -- "/api/v3/repos/$LOGIN/smoke/issues"
check "POST issue comment" --needs ISSUE_OK --data '{"body":"a comment"}' --status 201 \
  --jq '.body == "a comment" and (.id | type == "number")' -- "/api/v3/repos/$LOGIN/smoke/issues/1/comments"
check "PATCH issue (close)" --needs ISSUE_OK --method PATCH --data '{"state":"closed"}' --status 200 \
  --jq '.state == "closed" and (.closed_at | type == "string")' -- "/api/v3/repos/$LOGIN/smoke/issues/1"

echo "-- pulls, releases, search, notifications, orgs"
check "GET pulls" --needs REPO_OK --status 200 --jq 'type == "array"' -- "/api/v3/repos/$LOGIN/smoke/pulls"
check "GET releases" --needs REPO_OK --status 200 --jq 'type == "array"' -- "/api/v3/repos/$LOGIN/smoke/releases"
check "POST releases" --needs REPO_OK --data '{"tag_name":"v0.1.0","name":"v0.1.0"}' --status 201 \
  --jq '.tag_name == "v0.1.0" and (.upload_url | contains("{?name,label}"))' -- "/api/v3/repos/$LOGIN/smoke/releases"
check "search repositories" --status 200 --jq '.total_count >= 1 and (.items | type == "array")' \
  -- "/api/v3/search/repositories?q=smoke+user:$LOGIN"
check "search issues" --status 200 --jq '.total_count | type == "number"' \
  -- "/api/v3/search/issues?q=repo:$LOGIN/smoke+is:issue"
check "GET notifications" --status 200 --jq 'type == "array"' -- /api/v3/notifications
check "GET /user/orgs" --status 200 --jq 'type == "array"' -- /api/v3/user/orgs
check "GraphQL viewer" --data '{"query":"query { viewer { login } }"}' --status 200 \
  --jq ".data.viewer.login == \"$LOGIN\"" -- /api/graphql

echo "-- cleanup"
check "DELETE /repos/{owner}/{repo}" --method DELETE --status 204 -- "/api/v3/repos/$LOGIN/smoke-private"
check "deleted repo is 404" --status 404 -- "/api/v3/repos/$LOGIN/smoke-private"

report_finish api-smoke "$API_BASE" "$JSON_OUT" "{\"login\": \"$LOGIN\"}"
if [[ $STRICT == 1 ]] && report_failed; then
  exit 1
fi
exit 0
