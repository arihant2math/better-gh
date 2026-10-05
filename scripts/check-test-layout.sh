#!/usr/bin/env bash
# Integration tests are one binary per crate (crates/<crate>/tests/it/main.rs).
# Fails if a top-level tests/*.rs would add another (whole-server) test
# binary, or if a tests/it/*.rs file is not declared in its main.rs (it would
# silently never compile or run).
set -euo pipefail
cd "$(dirname "$0")/.."
status=0
for f in crates/*/tests/*.rs; do
  [ -e "$f" ] || continue
  echo "error: $f is a separate test binary; move it to $(dirname "$f")/it/ and declare it in main.rs" >&2
  status=1
done
for main in crates/*/tests/it/main.rs; do
  dir=$(dirname "$main")
  for f in "$dir"/*.rs "$dir"/*/mod.rs; do
    [ -e "$f" ] || continue
    case "$f" in */mod.rs) name=$(basename "$(dirname "$f")") ;; *) name=$(basename "$f" .rs) ;; esac
    [ "$name" = main ] && continue
    if ! grep -Eq "^(pub )?mod $name;" "$main"; then
      echo "error: $f is not declared in $main (add \`mod $name;\`)" >&2
      status=1
    fi
  done
done
exit $status
