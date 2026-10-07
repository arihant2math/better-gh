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
      # The image's dependency layer (cargo-chef) is keyed on the lockfile;
      # a bump is the backend change most likely to break the image build.
      Cargo.lock)
        backend=true docker=true ;;
      # Rust workspace, its fixtures, and the scripts the compat job runs.
      crates/* | migrations/* | scripts/* | .cargo/* | .config/nextest.toml \
        | Cargo.toml | rust-toolchain* | rustfmt.toml \
        | .rustfmt.toml | clippy.toml | .clippy.toml)
        backend=true ;;
      Dockerfile | .dockerignore)
        docker=true ;;
    esac
  done
fi

# The image builds the server and the embedded web client, but the full
# release build costs ~28 min, so PRs only run it for changes to the image
# itself (above); ordinary backend/frontend changes are covered by the other
# jobs and the image is still built on every push to main (`--all`).

echo "backend=$backend"
echo "frontend=$frontend"
echo "docker=$docker"
