#!/usr/bin/env bash
# Result bookkeeping shared by scripts/gh-compat.sh and scripts/api-smoke.sh:
# one PASS/FAIL/SKIP row per check, printed live, then a Markdown table and
# an optional JSON document.
#
# JSON shape:
#   {"generated_at", "tool", "server", "meta": {...},
#    "summary": {"pass", "fail", "skip", "total"},      (fixtures excluded)
#    "fixtures": {"name": "PASS"|"FAIL", ...},
#    "results": [{"kind", "name", "status", "exit_code", "duration_ms",
#                 "command", "detail"}, ...]}
# shellcheck shell=bash

report_init() {
  RESULTS="$WORK/results.tsv"
  : >"$RESULTS"
}

# Single line: tabs/newlines -> spaces, ANSI colors stripped, max 200 chars.
clean() { tr '\t\r\n' '   ' | sed -e 's/\x1b\[[0-9;]*m//g' -e 's/  */ /g' -e 's/^ //' | cut -c1-200; }

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# record KIND NAME STATUS EXIT_CODE MS COMMAND DETAIL
record() {
  local cmd detail
  cmd="$(printf '%s' "$6" | clean)"
  detail="$(printf '%s' "$7" | clean)"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" "$cmd" "$detail" >>"$RESULTS"
  local color="" reset=""
  if [[ -t 1 ]]; then
    reset=$'\e[0m'
    case "$3" in PASS) color=$'\e[32m' ;; FAIL) color=$'\e[31m' ;; *) color=$'\e[33m' ;; esac
  fi
  printf '%s%-4s%s  %-46s %6sms  %s\n' "$color" "$3" "$reset" "$2" "$5" "${detail:0:90}"
}

report_failed() { grep -q $'\tFAIL\t' "$RESULTS"; }

# report_finish TOOL SERVER JSON_OUT [META_JSON]
report_finish() {
  local meta="${4:-}"
  [[ -n $meta ]] || meta='{}'
  python3 - "$RESULTS" "$1" "$2" "${3:-}" "$meta" <<'EOF'
import datetime, json, sys
path, tool, server, json_out, meta = sys.argv[1:6]
rows = []
for line in open(path, encoding="utf-8", errors="replace"):
    kind, name, status, code, ms, cmd, detail = line.rstrip("\n").split("\t")
    rows.append({
        "kind": kind, "name": name, "status": status,
        "exit_code": int(code) if code else None, "duration_ms": int(ms),
        "command": cmd, "detail": detail,
    })
cases = [r for r in rows if r["kind"] != "fixture"]
summary = {s.lower(): sum(r["status"] == s for r in cases) for s in ("PASS", "FAIL", "SKIP")}
summary["total"] = len(cases)

print()
print("| Result | Check | Detail |")
print("|--------|-------|--------|")
for r in rows:
    detail = r["detail"].replace("|", "\\|")[:120]
    name = ("[fixture] " if r["kind"] == "fixture" else "") + r["name"]
    print(f"| {r['status']} | `{name}` | {detail} |")
print()
print(f"{tool}: {summary['pass']} passed, {summary['fail']} failed, "
      f"{summary['skip']} skipped (of {summary['total']})")

if json_out:
    doc = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "tool": tool,
        "server": server,
        "meta": json.loads(meta),
        "summary": summary,
        "fixtures": {r["name"]: r["status"] for r in rows if r["kind"] == "fixture"},
        "results": rows,
    }
    with open(json_out, "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    print(f"results written to {json_out}")
EOF
}
