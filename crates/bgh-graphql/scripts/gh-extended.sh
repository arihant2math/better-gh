#!/usr/bin/env bash
# Extended GitHub CLI matrix for the GraphQL layer (bgh-graphql): the
# fixtures of scripts/gh-compat.sh plus ~37 more `gh` commands (edit, close,
# lock, pin, develop, ready, checks, review, merge, archive, template,
# `--json` with every field, `gh api graphql --paginate`, node ids).
# Same options as scripts/gh-compat.sh.
#
# Original description: runs a matrix of real `gh` commands
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
source "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib/test-server.sh"
# shellcheck source=scripts/lib/report.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib/report.sh"

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
  api POST "/repos/$NWO/pulls" '{"title":"Fixture PR","head":"feature-pr","base":"main","body":"Created by gh-compat.sh"}'; then
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

# --- extended matrix -------------------------------------------------------------
echo "-- extended"
run_case "gh label create" --needs FX_REPO -- "$GH" label create compat-label -R "$NWO" --color 0e8a16
run_case "gh repo view --json all" --needs FX_REPO -- "$GH" repo view "$NWO" --json id,name,nameWithOwner,owner,parent,description,homepageUrl,url,sshUrl,createdAt,pushedAt,updatedAt,isArchived,isFork,isPrivate,visibility,stargazerCount,forkCount,defaultBranchRef,hasIssuesEnabled,licenseInfo,languages,primaryLanguage,repositoryTopics,latestRelease,viewerPermission,watchers,issues,pullRequests,labels,milestones,assignableUsers,mentionableUsers,diskUsage,isEmpty,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,deleteBranchOnMerge,viewerHasStarred,viewerSubscription,viewerCanAdminister
run_case "gh repo archive" --needs FX_DELREPO -- "$GH" repo archive "$OWNER/to-delete" --yes
run_case "gh repo unarchive" --needs FX_DELREPO -- "$GH" repo unarchive "$OWNER/to-delete" --yes
run_case "gh repo create --template" --needs FX_REPO -- bash -c '"$1" api -X PATCH "repos/$2" -F is_template=true >/dev/null && "$1" repo create "$3/from-template" --private --template "$2"' _ "$GH" "$NWO" "$OWNER"
run_case "gh issue create --label --assignee" --needs FX_REPO --expect "/issues/[0-9]+" -- "$GH" issue create -R "$NWO" --title "Labeled" --body b --label bug --assignee "$OWNER"
run_case "gh issue list filters" --needs FX_ISSUE -- "$GH" issue list -R "$NWO" --label bug --assignee "$OWNER" --state all --json number,title,labels,assignees
run_case "gh issue view --comments" --needs FX_ISSUE -- "$GH" issue view "$FX_ISSUE" -R "$NWO" --comments
run_case "gh issue view --json all" --needs FX_ISSUE -- "$GH" issue view "$FX_ISSUE" -R "$NWO" --json assignees,author,body,closed,comments,createdAt,closedAt,id,labels,milestone,number,projectCards,projectItems,reactionGroups,state,title,updatedAt,url,isPinned,stateReason
run_case "gh issue edit --title --add-assignee" --needs FX_ISSUE -- "$GH" issue edit "$FX_ISSUE" -R "$NWO" --title "Fixture issue (edited)" --add-assignee "$OWNER" --add-label compat-label
run_case "gh label edit" --needs FX_REPO -- "$GH" label edit compat-label -R "$NWO" --description "edited" --color 123456
run_case "gh label delete" --needs FX_REPO -- "$GH" label delete compat-label -R "$NWO" --yes
run_case "gh issue status" --needs FX_ISSUE -- "$GH" issue status -R "$NWO"
run_case "gh issue lock" --needs FX_ISSUE -- "$GH" issue lock "$FX_ISSUE" -R "$NWO"
run_case "gh issue unlock" --needs FX_ISSUE -- "$GH" issue unlock "$FX_ISSUE" -R "$NWO"
run_case "gh issue pin" --needs FX_ISSUE -- "$GH" issue pin "$FX_ISSUE" -R "$NWO"
run_case "gh issue unpin" --needs FX_ISSUE -- "$GH" issue unpin "$FX_ISSUE" -R "$NWO"
run_case "gh issue develop" --needs FX_ISSUE --needs FX_CLONE --in "${FX_CLONE:-$WORK}" -- "$GH" issue develop "$FX_ISSUE" -R "$NWO" --base main
run_case "gh issue develop --list" --needs FX_ISSUE --expect "^$FX_ISSUE-" -- "$GH" issue develop "$FX_ISSUE" -R "$NWO" --list
run_case "gh issue close --reason" --needs FX_ISSUE -- "$GH" issue close "$FX_ISSUE" -R "$NWO" --reason "not planned" --comment "closing"
run_case "gh pr create --draft" --needs FX_BRANCHES --expect "/pull/[0-9]+" -- "$GH" pr create -R "$NWO" --head feature-new --base main --title "Draft" --body x --draft
run_case "gh pr view --json all" --needs FX_PR -- "$GH" pr view "$FX_PR" -R "$NWO" --json additions,assignees,author,autoMergeRequest,baseRefName,body,changedFiles,closed,closedAt,comments,commits,createdAt,deletions,files,headRefName,headRefOid,headRepository,headRepositoryOwner,id,isCrossRepository,isDraft,labels,latestReviews,maintainerCanModify,mergeable,mergeCommit,mergedAt,mergedBy,mergeStateStatus,milestone,number,potentialMergeCommit,projectCards,reactionGroups,reviewDecision,reviewRequests,reviews,state,statusCheckRollup,title,updatedAt,url
run_case "gh pr view --comments" --needs FX_PR -- "$GH" pr view "$FX_PR" -R "$NWO" --comments
run_case "gh pr checks" --needs FX_PR -- bash -c 'out=$("$1" pr checks "$2" -R "$3" 2>&1); c=$?; echo "$out"; [ $c = 0 ] || [ $c = 8 ] || echo "$out" | grep -q "no checks reported"' _ "$GH" "$FX_PR" "$NWO"
run_case "gh pr edit" --needs FX_PR -- "$GH" pr edit "$FX_PR" -R "$NWO" --title "Fixture PR (edited)" --add-label bug --add-assignee "$OWNER"
run_case "gh pr ready --undo" --needs FX_PR -- "$GH" pr ready "$FX_PR" -R "$NWO" --undo
run_case "gh pr ready" --needs FX_PR -- "$GH" pr ready "$FX_PR" -R "$NWO"
run_case "gh pr review --approve (own PR fails)" -- bash -c '! "$1" pr review "$2" -R "$3" --approve >/dev/null 2>&1 || true' _ "$GH" "$FX_PR" "$NWO"
run_case "gh pr close" --needs FX_PR -- "$GH" pr close "$FX_PR" -R "$NWO"
run_case "gh pr reopen" --needs FX_PR -- "$GH" pr reopen "$FX_PR" -R "$NWO"
run_case "gh pr list --json" --needs FX_PR -- "$GH" pr list -R "$NWO" --state all --json number,title,headRefName,isDraft,reviewDecision,statusCheckRollup
run_case "gh pr list --search" --needs FX_PR -- "$GH" pr list -R "$NWO" --search "Fixture"
run_case "gh pr merge --squash --delete-branch" --needs FX_PR -- "$GH" pr merge "$FX_PR" -R "$NWO" --squash
run_case "gh search prs" --needs FX_PR -- "$GH" search prs "Fixture" --repo "$NWO"
run_case "gh release view --json" --needs FX_RELEASE -- "$GH" release view "$FX_RELEASE" -R "$NWO" --json tagName,name,assets,author,isDraft,isPrerelease,publishedAt,url
run_case "gh api graphql paginate" --needs FX_REPO -- "$GH" api graphql --paginate -f query='query($endCursor: String) { viewer { repositories(first: 1, after: $endCursor) { nodes { nameWithOwner } pageInfo { hasNextPage endCursor } } } }'
run_case "gh api graphql node" --needs FX_REPO -- bash -c 'id=$("$1" api "repos/$2" --jq .node_id) && "$1" api graphql -f query="query { node(id: \"$id\") { ... on Repository { nameWithOwner } } }" --jq .data.node.nameWithOwner | grep -q "$2"' _ "$GH" "$NWO"
run_case "gh status" -- "$GH" status

# --- report --------------------------------------------------------------------------
META="$(python3 -c 'import json,sys; print(json.dumps(dict(zip(["gh_version","gh_host","login"], sys.argv[1:]))))' \
  "$GH_VERSION" "$GH_HOST" "$OWNER")"
report_finish gh-extended "$API_BASE" "$JSON_OUT" "$META"

if [[ $STRICT == 1 ]] && report_failed; then
  exit 1
fi
exit 0
