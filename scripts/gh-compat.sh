#!/usr/bin/env bash
# GitHub CLI compatibility harness: runs a matrix of real `gh` commands
# against a bgh server in GitHub Enterprise Server mode and reports
# PASS / FAIL / SKIP per command.
#
#   scripts/gh-compat.sh                      start a throwaway server (temp DB + data dir)
#   scripts/gh-compat.sh --url https://git.example.com --token bghp_...   use a running server
#   scripts/gh-compat.sh --json results.json  also write machine-readable results
#
# Options:
#   --url URL        use a running server instead of starting one. https URLs
#                    are used as-is (see --cacert); for http URLs a local TLS
#                    proxy is put in front (gh only speaks https to GHES hosts).
#   --token TOKEN    PAT to use with --url (default: sign up a fresh account)
#   --cacert FILE    CA bundle that verifies --url (https)
#   --https-port N   port of the local TLS proxy (default: 443 when root and
#                    free, else random; 443 keeps GH_HOST port-less, which
#                    `gh pr checkout` and other remote-matching commands need)
#   --json FILE      write results as JSON
#   --filter REGEX   only run cases whose name matches (fixtures always run)
#   --strict         exit 1 when any case fails
#   --keep           keep the temp dir, database and server logs
#   -v, --verbose    print the output of failing commands
#
# Environment: DATABASE_URL / REDIS_URL (where to create the throwaway
# database; defaults match scripts/dev-setup.sh), BGH_BIN (prebuilt binary;
# otherwise `cargo build -p bgh-server`), GH (gh binary, default `gh`).
#
# How gh is pointed at bgh: GH_HOST=<host> + GH_ENTERPRISE_TOKEN=<pat> make
# gh treat the host as GitHub Enterprise Server, i.e. REST at
# https://<host>/api/v3/ and GraphQL at https://<host>/api/graphql. gh has no
# plain-http mode for enterprise hosts, so the harness terminates TLS in
# scripts/lib/tls-proxy.py with a throwaway CA trusted via SSL_CERT_FILE (gh)
# and http.sslCAInfo (git). gh's config/state live in the temp dir; your own
# ~/.config/gh is never touched.
# shellcheck disable=SC2034 # fixture flags (FX_*) are read via ${!var} in run_case
set -uo pipefail

TS_TAG=gh-compat
# shellcheck source=scripts/lib/test-server.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-server.sh"
# shellcheck source=scripts/lib/report.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/report.sh"

URL=""
TOKEN=""
CACERT=""
HTTPS_PORT=auto
JSON_OUT=""
FILTER=""
STRICT=0
VERBOSE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --url) URL="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --cacert) CACERT="$2"; shift 2 ;;
    --https-port) HTTPS_PORT="$2"; shift 2 ;;
    --json) JSON_OUT="$2"; shift 2 ;;
    --filter) FILTER="$2"; shift 2 ;;
    --strict) STRICT=1; shift ;;
    --keep) TS_KEEP=1; shift ;;
    -v|--verbose) VERBOSE=1; shift ;;
    -h|--help) awk 'NR==1{next} !/^#/{exit} /shellcheck/{next} {sub(/^# ?/,""); print}' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done
[[ -n $JSON_OUT && $JSON_OUT != /* ]] && JSON_OUT="$PWD/$JSON_OUT"

GH="${GH:-gh}"
ts_need "$GH" git curl python3

ts_mkwork
trap ts_cleanup EXIT
trap 'exit 130' INT TERM

if [[ -n $URL ]]; then
  ts_use_server "$URL" 1 "$CACERT"
else
  ts_start_server 1 "$HTTPS_PORT"
fi
if [[ -n $TOKEN ]]; then
  TS_TOKEN="$TOKEN"
  TS_LOGIN="$(ts_login_for_token)" || ts_die "token rejected by GET /user"
else
  ts_create_account
fi
OWNER="$TS_LOGIN"
REPO="compat"
NWO="$OWNER/$REPO"

# --- isolated gh + git environment ---------------------------------------------
mkdir -p "$WORK/gh" "$WORK/xdg" "$WORK/home" "$WORK/runs"
export GH_CONFIG_DIR="$WORK/gh"
export XDG_CONFIG_HOME="$WORK/xdg/config" XDG_STATE_HOME="$WORK/xdg/state"
export XDG_CACHE_HOME="$WORK/xdg/cache" XDG_DATA_HOME="$WORK/xdg/data"
export GH_HOST="$TS_HOST"
export GH_ENTERPRISE_TOKEN="$TS_TOKEN"
export GH_PROMPT_DISABLED=1 GH_NO_UPDATE_NOTIFIER=1 GH_NO_EXTENSION_UPDATE_NOTIFIER=1
export GH_SPINNER_DISABLED=1 NO_COLOR=1 CLICOLOR=0 GH_PAGER=cat PAGER=cat GIT_PAGER=cat
export NO_PROXY="localhost,127.0.0.1${NO_PROXY:+,$NO_PROXY}" no_proxy="localhost,127.0.0.1${no_proxy:+,$no_proxy}"
unset GH_TOKEN GITHUB_TOKEN GH_REPO
if [[ -n ${TS_CA_FILE:-} ]]; then
  export SSL_CERT_FILE="$TS_CA_FILE" GIT_SSL_CAINFO="$TS_CA_FILE"
fi
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$WORK/gitconfig" GIT_TERMINAL_PROMPT=0
cat >"$GIT_CONFIG_GLOBAL" <<EOF
[user]
	name = gh compat
	email = $OWNER@example.com
[init]
	defaultBranch = main
[advice]
	detachedHead = false
EOF
# Credentials for fixture setup only (gh's own helper is configured by the
# `gh auth setup-git` case and used by `gh repo clone` / `gh pr checkout`).
cat >"$WORK/cred-helper.sh" <<'EOF'
#!/bin/sh
[ "$1" = get ] && printf 'username=%s\npassword=%s\n' "$BGH_COMPAT_LOGIN" "$BGH_COMPAT_TOKEN"
exit 0
EOF
chmod +x "$WORK/cred-helper.sh"
export BGH_COMPAT_LOGIN="$OWNER" BGH_COMPAT_TOKEN="$TS_TOKEN"

# --- result bookkeeping ------------------------------------------------------------
report_init
N=0

# run_case NAME [--needs VAR]... [--expect REGEX] [--in DIR] -- COMMAND...
#   PASS: exit 0 (and stdout matches --expect). SKIP: a --needs fixture
#   variable is empty. Every command gets a 60s timeout.
run_case() {
  local name="$1"; shift
  local needs=() expect="" dir="$WORK/runs" kind=gh
  while [[ $# -gt 0 && $1 != -- ]]; do
    case "$1" in
      --needs) needs+=("$2"); shift 2 ;;
      --expect) expect="$2"; shift 2 ;;
      --in) dir="$2"; shift 2 ;;
      --kind) kind="$2"; shift 2 ;;
      *) echo "run_case: bad option $1" >&2; return 2 ;;
    esac
  done
  shift
  if [[ $kind == gh && -n $FILTER ]] && ! [[ $name =~ $FILTER ]]; then
    return 0
  fi
  local cmd="${*//$TS_TOKEN/***}" v
  for v in "${needs[@]}"; do
    if [[ -z ${!v:-} ]]; then
      record "$kind" "$name" SKIP "" 0 "$cmd" "fixture unavailable: $v"
      return 0
    fi
  done
  N=$((N + 1))
  local out="$WORK/out.$N" err="$WORK/err.$N" start code ms status detail
  start=$(now_ms)
  if declare -F "$1" >/dev/null; then
    (cd "$dir" && "$@") >"$out" 2>"$err" </dev/null # shell function (curl has its own timeout)
  else
    (cd "$dir" && timeout 60 "$@") >"$out" 2>"$err" </dev/null
  fi
  code=$?
  ms=$(($(now_ms) - start))
  status=PASS detail=""
  if [[ $code != 0 ]]; then
    status=FAIL
    detail="$( (grep -v '^\s*$' "$err" || true) | tail -n 1)"
    [[ -z $detail ]] && detail="$( (grep -v '^\s*$' "$out" || true) | tail -n 1)"
    [[ $code == 124 ]] && detail="timed out after 60s${detail:+; $detail}"
    [[ -z $detail ]] && detail="exit $code"
  elif [[ -n $expect ]] && ! grep -Eq -- "$expect" "$out"; then
    status=FAIL
    detail="output did not match /$expect/: $(head -c 300 "$out")"
  fi
  record "$kind" "$name" "$status" "$code" "$ms" "$cmd" "$detail"
  if [[ $status == FAIL && $VERBOSE == 1 ]]; then
    sed 's/^/      | /' "$err" "$out" | head -n 40
  fi
  LAST_OUT="$out"
  [[ $status == PASS ]]
}

# shellcheck disable=SC2329 # invoked through run_case
api() { # METHOD PATH [JSON] -> body on stdout, fails on non-2xx
  local args=(-f -X "$1" -H "authorization: token $TS_TOKEN" -H "accept: application/vnd.github+json")
  [[ $# -ge 3 ]] && args+=(-H "content-type: application/json" -d "$3")
  ts_curl "${args[@]}" "$API_BASE/api/v3$2"
}
jfield() { python3 -c 'import json,sys; v=json.load(sys.stdin); print(v.get(sys.argv[1], ""))' "$1"; }

GH_VERSION="$("$GH" --version | head -n 1)"
echo "== gh compatibility: $GH_VERSION against $API_BASE (GH_HOST=$GH_HOST) as $OWNER"

# --- fixtures (REST + git, independent of gh) -----------------------------------
FX_REPO="" FX_BRANCHES="" FX_ISSUE="" FX_PR="" FX_RELEASE="" FX_CLONE="" FX_DELREPO=""
if run_case "create repo $NWO (REST)" --kind fixture -- \
  api POST /user/repos '{"name":"compat","description":"gh compatibility fixture","auto_init":true}'; then
  FX_REPO=1
fi
# shellcheck disable=SC2016 # expanded by the inner bash
if [[ -n $FX_REPO ]] && run_case "clone + push branches (git)" --kind fixture -- bash -c '
    set -e
    fixture_git() { git -c credential.helper= -c "credential.helper=$1/cred-helper.sh" "${@:2}"; }
    fixture_git "$1" clone -q "$2" "$1/fixture"
    cd "$1/fixture"
    git checkout -q -b feature-pr
    echo "change for the fixture PR" >pr.txt && git add pr.txt && git commit -qm "Fixture PR change"
    git checkout -q main && git checkout -q -b feature-new
    echo "change for gh pr create" >new.txt && git add new.txt && git commit -qm "Change for gh pr create"
    git checkout -q main
    fixture_git "$1" push -q origin feature-pr feature-new
  ' _ "$WORK" "$API_BASE/$NWO.git"; then
  FX_BRANCHES=1
  FX_CLONE="$WORK/fixture"
fi
if [[ -n $FX_REPO ]] && run_case "create issue (REST)" --kind fixture -- \
  api POST "/repos/$NWO/issues" '{"title":"Fixture issue","body":"Created by gh-compat.sh"}'; then
  FX_ISSUE="$(jfield number <"$LAST_OUT")"
fi
if [[ -n $FX_BRANCHES ]] && run_case "create pull request (REST)" --kind fixture -- \
  api POST "/repos/$NWO/pulls" "{\"title\":\"Fixture PR\",\"head\":\"feature-pr\",\"base\":\"main\",\"body\":\"Created by gh-compat.sh${FX_ISSUE:+. Fixes #$FX_ISSUE}\"}"; then
  FX_PR="$(jfield number <"$LAST_OUT")"
fi
if [[ -n $FX_REPO ]] && run_case "create release v0.0.1 (REST)" --kind fixture -- \
  api POST "/repos/$NWO/releases" '{"tag_name":"v0.0.1","name":"v0.0.1","body":"fixture"}'; then
  FX_RELEASE=v0.0.1
fi
if run_case "create repo $OWNER/to-delete (REST)" --kind fixture -- \
  api POST /user/repos '{"name":"to-delete"}'; then
  FX_DELREPO=1
fi
echo "release asset" >"$WORK/asset.txt"

# --- the gh matrix ------------------------------------------------------------------
echo "-- auth & api"
run_case "gh auth status" -- "$GH" auth status --hostname "$GH_HOST"
run_case "gh auth setup-git" -- "$GH" auth setup-git --hostname "$GH_HOST"
run_case "gh api user" --expect "^$OWNER\$" -- "$GH" api user --jq .login
run_case "gh api repos/{owner}/{repo}" --needs FX_REPO --expect "^$NWO\$" -- \
  "$GH" api "repos/$NWO" --jq .full_name
run_case "gh api --paginate user/repos" --needs FX_REPO --expect "^$REPO\$" -- \
  "$GH" api --paginate "user/repos?per_page=1" --jq '.[].name'
run_case "gh api graphql viewer" --expect "^$OWNER\$" -- \
  "$GH" api graphql -f query='query { viewer { login } }' --jq .data.viewer.login

echo "-- repos"
run_case "gh repo create" --expect "gh-created" -- \
  "$GH" repo create "$OWNER/gh-created" --public --description "created by gh"
run_case "gh repo view" --needs FX_REPO --expect "$REPO" -- "$GH" repo view "$NWO"
run_case "gh repo view --json" --needs FX_REPO --expect "^main\$" -- \
  "$GH" repo view "$NWO" --json name,owner,defaultBranchRef --jq .defaultBranchRef.name
run_case "gh repo list" --needs FX_REPO --expect "$NWO" -- "$GH" repo list "$OWNER" --limit 20
run_case "gh repo clone" --needs FX_REPO -- "$GH" repo clone "$NWO" "$WORK/runs/gh-clone"
run_case "gh repo edit --description" --needs FX_REPO -- \
  "$GH" repo edit "$NWO" --description "edited by gh"

echo "-- labels"
run_case "gh label list" --needs FX_REPO -- "$GH" label list -R "$NWO"
run_case "gh label create" --needs FX_REPO -- \
  "$GH" label create compat-label -R "$NWO" --color 0e8a16 --description "created by gh"

echo "-- issues"
run_case "gh issue create" --needs FX_REPO --expect "/issues/[0-9]+" -- \
  "$GH" issue create -R "$NWO" --title "Created by gh" --body "issue body"
run_case "gh issue list" --needs FX_ISSUE --expect "Fixture issue" -- "$GH" issue list -R "$NWO"
run_case "gh issue list --json" --needs FX_ISSUE --expect "Fixture issue" -- \
  "$GH" issue list -R "$NWO" --state all --json number,title,state --jq '.[].title'
run_case "gh issue view" --needs FX_ISSUE --expect "Fixture issue" -- "$GH" issue view "$FX_ISSUE" -R "$NWO"
run_case "gh issue comment" --needs FX_ISSUE -- \
  "$GH" issue comment "$FX_ISSUE" -R "$NWO" --body "comment from gh"
run_case "gh issue edit --add-label" --needs FX_ISSUE -- \
  "$GH" issue edit "$FX_ISSUE" -R "$NWO" --add-label compat-label
run_case "gh issue close" --needs FX_ISSUE -- "$GH" issue close "$FX_ISSUE" -R "$NWO"
run_case "gh issue reopen" --needs FX_ISSUE -- "$GH" issue reopen "$FX_ISSUE" -R "$NWO"

echo "-- pull requests"
run_case "gh pr create" --needs FX_BRANCHES --expect "/pull/[0-9]+" -- \
  "$GH" pr create -R "$NWO" --head feature-new --base main --title "Created by gh" --body "pr body"
run_case "gh pr list" --needs FX_PR --expect "Fixture PR" -- "$GH" pr list -R "$NWO"
run_case "gh pr view" --needs FX_PR --expect "Fixture PR" -- "$GH" pr view "$FX_PR" -R "$NWO"
run_case "gh pr view --json" --needs FX_PR --expect "^feature-pr\$" -- \
  "$GH" pr view "$FX_PR" -R "$NWO" --json number,state,headRefName --jq .headRefName
run_case "gh pr view --json closingIssuesReferences" --needs FX_PR --needs FX_ISSUE --expect "^${FX_ISSUE:-x}\$" -- \
  "$GH" pr view "$FX_PR" -R "$NWO" --json closingIssuesReferences --jq '.closingIssuesReferences[].number'
run_case "gh pr diff" --needs FX_PR --expect "pr.txt" -- "$GH" pr diff "$FX_PR" -R "$NWO"
run_case "gh pr checkout" --needs FX_PR --needs FX_CLONE --in "${FX_CLONE:-$WORK}" -- \
  "$GH" pr checkout "$FX_PR"
run_case "gh pr status" --needs FX_REPO -- "$GH" pr status -R "$NWO"
run_case "gh pr comment" --needs FX_PR -- "$GH" pr comment "$FX_PR" -R "$NWO" --body "comment from gh"
run_case "gh pr review --comment" --needs FX_PR -- \
  "$GH" pr review "$FX_PR" -R "$NWO" --comment --body "review from gh"
run_case "gh pr merge --merge" --needs FX_PR -- "$GH" pr merge "$FX_PR" -R "$NWO" --merge

echo "-- projects"
FX_PROJECT=""
if run_case "gh project create" --expect "/users/$OWNER/projects/[0-9]+" -- \
  "$GH" project create --owner "$OWNER" --title "Compat board" --format json --jq .url; then
  FX_PROJECT="$(grep -Eo 'projects/[0-9]+' "$LAST_OUT" | head -n 1 | cut -d/ -f2)"
fi
run_case "gh project list" --needs FX_PROJECT --expect "Compat board" -- \
  "$GH" project list --owner "$OWNER"
run_case "gh project view" --needs FX_PROJECT --expect "Compat board" -- \
  "$GH" project view "${FX_PROJECT:-0}" --owner "$OWNER"
run_case "gh project field-list" --needs FX_PROJECT --expect "Status" -- \
  "$GH" project field-list "${FX_PROJECT:-0}" --owner "$OWNER"
run_case "gh project field-create" --needs FX_PROJECT --expect "Priority" -- \
  "$GH" project field-create "${FX_PROJECT:-0}" --owner "$OWNER" --name Priority \
  --data-type SINGLE_SELECT --single-select-options High,Low --format json --jq .name
FX_ITEM="" FX_PROJECT_ID="" FX_STATUS_FIELD="" FX_DONE_OPTION=""
if [[ -n $FX_PROJECT && -n $FX_ISSUE ]] && run_case "gh project item-add" -- \
  "$GH" project item-add "$FX_PROJECT" --owner "$OWNER" \
  --url "https://$GH_HOST/$NWO/issues/$FX_ISSUE" --format json --jq .id; then
  FX_ITEM="$(tr -d '[:space:]' <"$LAST_OUT")"
fi
if [[ -n $FX_PROJECT ]] && run_case "gh project view --format json" --kind fixture -- \
  "$GH" project view "$FX_PROJECT" --owner "$OWNER" --format json --jq .id; then
  FX_PROJECT_ID="$(tr -d '[:space:]' <"$LAST_OUT")"
fi
if [[ -n $FX_PROJECT ]] && run_case "gh project field-list --format json" --kind fixture -- \
  "$GH" project field-list "$FX_PROJECT" --owner "$OWNER" --format json \
  --jq '.fields[] | select(.name == "Status") | .id + " " + (.options[] | select(.name == "Done") | .id)'; then
  read -r FX_STATUS_FIELD FX_DONE_OPTION <"$LAST_OUT"
fi
run_case "gh project item-list" --needs FX_ITEM --expect "Fixture issue" -- \
  "$GH" project item-list "${FX_PROJECT:-0}" --owner "$OWNER"
run_case "gh project item-edit" --needs FX_ITEM --needs FX_PROJECT_ID --needs FX_DONE_OPTION -- \
  "$GH" project item-edit --id "${FX_ITEM:-x}" --project-id "${FX_PROJECT_ID:-x}" \
  --field-id "${FX_STATUS_FIELD:-x}" --single-select-option-id "${FX_DONE_OPTION:-x}"
run_case "gh project item-list --format json" --needs FX_ITEM --expect "^Done\$" -- \
  "$GH" project item-list "${FX_PROJECT:-0}" --owner "$OWNER" --format json --jq '.items[0].status'
run_case "gh project item-create" --needs FX_PROJECT -- \
  "$GH" project item-create "${FX_PROJECT:-0}" --owner "$OWNER" --title "Draft from gh" --body "draft body"
run_case "gh project item-archive" --needs FX_ITEM -- \
  "$GH" project item-archive "${FX_PROJECT:-0}" --owner "$OWNER" --id "${FX_ITEM:-x}"
run_case "gh issue create --project" --needs FX_PROJECT --expect "/issues/[0-9]+" -- \
  "$GH" issue create -R "$NWO" --title "Tracked by gh" --body "with project" --project "Compat board"
run_case "gh issue create --project (item added)" --needs FX_PROJECT --expect "Tracked by gh" -- \
  "$GH" project item-list "${FX_PROJECT:-0}" --owner "$OWNER" --format json --jq '.items[].content.title'
run_case "gh project close" --needs FX_PROJECT -- \
  "$GH" project close "${FX_PROJECT:-0}" --owner "$OWNER"

echo "-- actions"
FX_DISPATCH=""
# shellcheck disable=SC2016 # expanded by the inner bash
if [[ -n $FX_REPO ]] && run_case "push repository_dispatch workflow (git)" --kind fixture -- bash -c '
    set -e
    fixture_git() { git -c credential.helper= -c "credential.helper=$1/cred-helper.sh" "${@:2}"; }
    fixture_git "$1" clone -q "$2" "$1/dispatch"
    cd "$1/dispatch"
    mkdir -p .github/workflows
    printf "on:\n  repository_dispatch:\n    types: [deploy]\njobs:\n  d:\n    runs-on: none\n    steps: [{run: echo}]\n" >.github/workflows/dispatch.yml
    git add -A && git -c user.name=gh -c user.email=gh@example.com commit -qm "Add dispatch workflow"
    fixture_git "$1" push -q origin HEAD:main
  ' _ "$WORK" "$API_BASE/$NWO.git"; then
  FX_DISPATCH=1
fi
run_case "gh api -X POST repos/{owner}/{repo}/dispatches" --needs FX_DISPATCH -- \
  "$GH" api -X POST "repos/$NWO/dispatches" -f event_type=deploy -F 'client_payload[env]=prod'
# shellcheck disable=SC2329 # invoked through run_case
dispatch_run() { # the dispatch starts a repository_dispatch run (async trigger)
  local i
  for i in $(seq 1 30); do
    "$GH" api "repos/$NWO/actions/runs?event=repository_dispatch" --jq '.workflow_runs[].event' | grep -q repository_dispatch &&
      { echo repository_dispatch; return 0; }
    sleep 1
  done
  return 1
}
run_case "gh api actions/runs?event=repository_dispatch" --needs FX_DISPATCH --expect "^repository_dispatch\$" -- \
  dispatch_run

echo "-- releases"
run_case "gh release create" --needs FX_REPO --expect "/releases/tag/v1\\.0\\.0" -- \
  "$GH" release create v1.0.0 -R "$NWO" --title "v1.0.0" --notes "notes from gh"
run_case "gh release list" --needs FX_RELEASE --expect "v0\\.0\\.1" -- "$GH" release list -R "$NWO"
run_case "gh release view" --needs FX_RELEASE --expect "v0\\.0\\.1" -- "$GH" release view "$FX_RELEASE" -R "$NWO"
run_case "gh release upload" --needs FX_RELEASE -- \
  "$GH" release upload "$FX_RELEASE" "$WORK/asset.txt" -R "$NWO"

echo "-- search & misc"
run_case "gh search repos" --needs FX_REPO --expect "$NWO" -- "$GH" search repos "$REPO" --owner "$OWNER"
run_case "gh search issues" --needs FX_ISSUE --expect "Fixture issue" -- \
  "$GH" search issues "Fixture" --repo "$NWO"
run_case "gh status" -- "$GH" status
run_case "gh repo delete" --needs FX_DELREPO -- "$GH" repo delete "$OWNER/to-delete" --yes

# --- report --------------------------------------------------------------------------
META="$(python3 -c 'import json,sys; print(json.dumps(dict(zip(["gh_version","gh_host","login"], sys.argv[1:]))))' \
  "$GH_VERSION" "$GH_HOST" "$OWNER")"
report_finish gh-compat "$API_BASE" "$JSON_OUT" "$META"

if [[ $STRICT == 1 ]] && report_failed; then
  exit 1
fi
exit 0
