#!/usr/bin/env bash
# End-to-end check of the Actions UI against the real backend: starts a
# throwaway bgh server (temp DB + data dir) serving web/dist with the built-in
# runner on the shell executor, pushes a repository with a small workflow,
# waits for the run, then drives the web UI with Playwright
# (web/scripts/actions-e2e.mjs): runs list, run graph, logs (live + done),
# dispatch form, secrets (sealed box, verified by a job), runners.
#
#   scripts/actions-e2e.sh [--shots DIR] [--keep]
#
# Needs: postgres + redis (scripts/dev-setup.sh), a built web client
# (cd web && npm run build), Playwright with Chromium
# (PLAYWRIGHT_BROWSERS_PATH, preinstalled in CI images).
set -uo pipefail

TS_TAG=actions-e2e
# shellcheck source=scripts/lib/test-server.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/test-server.sh"

SHOTS="${TMPDIR:-/tmp}/bgh-actions-shots"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --shots) SHOTS="$2"; shift 2 ;;
    --keep) TS_KEEP=1; shift ;;
    *) ts_die "unknown option: $1" ;;
  esac
done

[[ -f $TS_ROOT/web/dist/index.html ]] || ts_die "web client not built: cd web && npm run build"
ts_need git node
ts_mkwork
trap ts_cleanup EXIT

TS_EXTRA_ENV=(
  "BGH_WEB_DIR=$TS_ROOT/web/dist"
  "BGH_ACTIONS_BUILTIN_RUNNER=true"
  "BGH_ACTIONS_EXECUTOR=shell"
  "BGH_ACTIONS_MAX_JOBS=3"
  "BGH_ACTIONS_REMOTE_ACTIONS=false"
)
ts_start_server 0
ts_create_account e2e
REPO=demo
api() { ts_curl -f -H "authorization: token $TS_TOKEN" -H 'content-type: application/json' "$@"; }

api -d "{\"name\":\"$REPO\",\"auto_init\":false}" "$API_BASE/api/v3/user/repos" >/dev/null ||
  ts_die "creating the repository failed"

SRC="$WORK/src"
mkdir -p "$SRC/.github/workflows" "$SRC/src"
cat >"$SRC/.github/workflows/ci.yml" <<'YAML'
name: CI
run-name: "CI on ${{ github.ref_name }}"
on:
  push:
  workflow_dispatch:
    inputs:
      greeting:
        description: Greeting to print
        default: hello
      level:
        description: Log level
        type: choice
        options: [info, debug]
        default: info
      slow:
        description: Stream slowly
        type: boolean
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Lint
        run: |
          printf '\033[32m✔ formatting\033[0m \033[1;33mok\033[0m\n'
          echo "::notice title=Lint::All files formatted"
      - name: Environment
        run: env | sort
  build:
    needs: lint
    runs-on: ubuntu-latest
    strategy:
      matrix:
        variant: [debug, release]
    steps:
      - uses: actions/checkout@v4
      - name: Compile
        run: |
          echo "::group::Compile ${{ matrix.variant }}"
          for i in $(seq 1 400); do echo "compiling unit $i of 400"; done
          echo "::endgroup::"
          echo "::warning file=src/main.rs,line=3::unused variable \`x\`"
      - run: mkdir -p out && echo "binary ${{ matrix.variant }}" > out/app.txt
      - uses: actions/upload-artifact@v4
        with:
          name: app-${{ matrix.variant }}
          path: out
  test:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - name: Unit tests
        env:
          SLOW: ${{ inputs.slow }}
        run: |
          echo "${{ inputs.greeting || 'hi' }} (${{ inputs.level || 'info' }})"
          for i in $(seq 1 12); do
            echo "test case $i ... ok"
            if [ "$SLOW" = "true" ]; then sleep 1; fi
          done
  secrets:
    needs: lint
    if: github.event_name == 'workflow_dispatch'
    runs-on: ubuntu-latest
    steps:
      - name: Secret is decrypted
        env:
          S: ${{ secrets.E2E_SECRET }}
        run: |
          test "$S" = "sealed-box-works" && echo "secret matches: $S"
  flaky:
    needs: lint
    if: github.event_name == 'push'
    runs-on: ubuntu-latest
    steps:
      - name: Integration test
        run: |
          echo "starting integration test"
          echo "::error file=tests/api.rs,line=42::expected 200, got 500"
          exit 1
  deploy:
    needs: [test, flaky]
    runs-on: ubuntu-latest
    steps:
      - run: echo deploying
YAML
printf 'fn main() {\n    let x = 1;\n}\n' >"$SRC/src/main.rs"
printf '# demo\n' >"$SRC/README.md"
(
  cd "$SRC" &&
    git init -q -b main &&
    git -c user.name=E2E -c user.email=e2e@example.com add -A &&
    git -c user.name=E2E -c user.email=e2e@example.com commit -qm "Add CI workflow" &&
    git push -q "http://$TS_LOGIN:$TS_TOKEN@${API_BASE#http://}/$TS_LOGIN/$REPO.git" main
) || ts_die "git push failed"

# A pull request from `feature` (its push run reports checks on the PR head;
# the UI re-runs one from the Checks tab).
(
  cd "$SRC" &&
    git checkout -q -b feature &&
    printf 'fn main() {}\n' >"src/main.rs" &&
    git -c user.name=E2E -c user.email=e2e@example.com commit -qam "Simplify main" &&
    git push -q "http://$TS_LOGIN:$TS_TOKEN@${API_BASE#http://}/$TS_LOGIN/$REPO.git" feature
) || ts_die "git push feature failed"
api -d '{"title":"Simplify main","head":"feature","base":"main"}' "$API_BASE/api/v3/repos/$TS_LOGIN/$REPO/pulls" >/dev/null ||
  ts_die "creating the pull request failed"

# Wait for the push runs to finish (the built-in runner picks them up).
ts_log "waiting for the push runs"
for _ in $(seq 1 180); do
  st="$(api "$API_BASE/api/v3/repos/$TS_LOGIN/$REPO/actions/runs" |
    python3 -c 'import json,sys; r=json.load(sys.stdin)["workflow_runs"]; print("completed" if len(r) >= 2 and all(x["status"] == "completed" for x in r) else "waiting")')"
  [[ $st == completed ]] && break
  sleep 1
done
[[ $st == completed ]] || { tail -n 40 "$WORK/server.log" >&2; ts_die "push run did not complete (status: $st)"; }

# Steps must not see the server's environment (P8 runner isolation).
lint_job="$(api "$API_BASE/api/v3/repos/$TS_LOGIN/$REPO/actions/runs" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["workflow_runs"][0]["id"])')"
lint_job="$(api "$API_BASE/api/v3/repos/$TS_LOGIN/$REPO/actions/runs/$lint_job/jobs" |
  python3 -c 'import json,sys; print(next(j["id"] for j in json.load(sys.stdin)["jobs"] if j["name"] == "lint"))')"
lint_log="$(ts_curl -fL -H "authorization: token $TS_TOKEN" "$API_BASE/api/v3/repos/$TS_LOGIN/$REPO/actions/jobs/$lint_job/logs")" ||
  ts_die "fetching the lint job log failed"
grep -q 'GITHUB_REPOSITORY=' <<<"$lint_log" || ts_die "the env step printed nothing"
if grep -E 'DATABASE_URL|REDIS_URL|BGH_|SMTP' <<<"$lint_log" >&2; then
  ts_die "server environment leaked into a workflow step"
fi
ts_log "step environment is isolated"

mkdir -p "$SHOTS"
PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers}" \
  node "$TS_ROOT/web/scripts/actions-e2e.mjs" "$API_BASE" "$TS_LOGIN" "$TS_PASSWORD" "$REPO" "$SHOTS" "$TS_TOKEN"
status=$?
[[ $status == 0 ]] || tail -n 60 "$WORK/server.log" >&2
ts_log "screenshots: $SHOTS"
exit $status
