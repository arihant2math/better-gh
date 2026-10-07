#!/usr/bin/env bash
# Classify changed paths (one per line on stdin) into the CI job groups and
# print `backend=`, `frontend=` and `docker=` lines (true/false) for
# $GITHUB_OUTPUT. `--all` sets everything without reading stdin.
#
# Keep the dependency rules in sync with the header of
# .github/workflows/ci.yml and "CI is the shared gate" in
# docs/AGENT_WORKFLOW.md.
#
#   git diff --name-only HEAD^1 HEAD | scripts/ci-changes.sh
set -euo pipefail

backend=false frontend=false docker=false

if [[ "${1:-}" == --all ]]; then
  backend=true frontend=true docker=true
else
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    case "$path" in
      # CI definition itself: run everything.
      .github/workflows/* | .github/actions/* | scripts/ci-changes.sh)
        backend=true frontend=true docker=true ;;
      # Shared by both sides: bgh-core include_str!s emoji.json, and the
      # markdown golden corpus is read by Rust tests and the web vitest.
      web/src/ui/markdown/emoji.json | testdata/*)
        backend=true frontend=true ;;
      web/*)
        frontend=true ;;
      # Rust workspace, its fixtures, and the scripts the compat job runs.
      crates/* | migrations/* | scripts/* | .cargo/* \
        | Cargo.toml | Cargo.lock | rust-toolchain* | rustfmt.toml \
        | .rustfmt.toml | clippy.toml | .clippy.toml)
        backend=true ;;
      Dockerfile | .dockerignore)
        docker=true ;;
    esac
  done
fi

# The image builds both the server and the embedded web client.
if [[ "$backend" == true || "$frontend" == true ]]; then
  docker=true
fi

echo "backend=$backend"
echo "frontend=$frontend"
echo "docker=$docker"
