#!/usr/bin/env bash
# Migration numbering guard (docs/ARCHITECTURE.md "Migrations").
#
#   scripts/check-migrations.sh [BASE]   # BASE defaults to origin/main
#
# sqlx applies every unapplied version regardless of order, so a migration
# numbered below one already on main would run after it on upgraded installs
# but before it on fresh ones. Fails if, relative to BASE:
# * a migration that exists at merge-base(BASE, HEAD) was edited, renamed
#   or deleted in the working tree (merged migrations are immutable);
# * an added migration's version is not greater than every version on BASE;
# * any file in migrations/ is misnamed or two files share a version.
# In CI on a pull_request, BASE is HEAD^1 (the base tip of the merge commit).
set -euo pipefail
cd "$(dirname "$0")/.."

base=${1:-origin/main}
git rev-parse -q --verify "$base^{commit}" >/dev/null || {
  echo "error: base '$base' not found (git fetch origin main?)" >&2
  exit 2
}
fork=$(git merge-base "$base" HEAD)
status=0

# Version of a migration path, as a plain integer (leading zeros stripped).
version() {
  local v
  v=$(basename "$1")
  v=${v%%_*}
  echo $((10#$v))
}

# Names and uniqueness in the working tree.
declare -A seen=()
for f in migrations/*; do
  name=$(basename "$f")
  if ! [[ $name =~ ^[0-9]{4,}_[a-z0-9_]+\.sql$ ]]; then
    echo "error: $f: expected migrations/NNNN_description.sql (lowercase, digits, _)" >&2
    status=1
    continue
  fi
  v=$(version "$name")
  if [ -n "${seen[$v]:-}" ]; then
    echo "error: $f: version $v is also used by ${seen[$v]}" >&2
    status=1
  fi
  seen[$v]=$f
done

# Merged migrations are immutable. Compares against the working tree, so
# uncommitted edits count too.
declare -A merged=()
while read -r path; do
  [ -n "$path" ] || continue
  merged[$path]=1
  if [ ! -e "$path" ]; then
    echo "error: $path: merged migrations must not be deleted or renamed; add a new migration instead" >&2
    status=1
  elif ! git diff --quiet "$fork" -- "$path"; then
    echo "error: $path: merged migrations must not be edited; add a new migration instead" >&2
    status=1
  fi
done < <(git ls-tree --name-only "$fork" migrations/)

# Added migrations sort after everything on BASE.
max=0
while read -r path; do
  [[ $(basename "$path") =~ ^[0-9]+_ ]] || continue
  v=$(version "$path")
  if [ "$v" -gt "$max" ]; then max=$v; fi
done < <(git ls-tree --name-only "$base" migrations/)

for path in migrations/*; do
  [ -z "${merged[$path]:-}" ] || continue
  [[ $(basename "$path") =~ ^[0-9]+_ ]] || continue
  v=$(version "$path")
  if [ "$v" -le "$max" ]; then
    echo "error: $path: version $v must be greater than $max, the highest on $base; renumber it (e.g. to $((max + 10)))" >&2
    status=1
  fi
done

[ "$status" -eq 0 ] && echo "migrations ok (highest on $base: $max)"
exit $status
