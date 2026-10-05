#!/usr/bin/env bash
# Re-vendor the license (choosealicense.com, MIT) and .gitignore
# (github/gitignore, CC0-1.0) templates served by /licenses and
# /gitignore/templates. The server never fetches them at runtime.
set -euo pipefail
cd "$(dirname "$0")/.."
RAW=https://raw.githubusercontent.com

LIC_DIR=crates/bgh-core/data/licenses
mkdir -p "$LIC_DIR"
for key in 0bsd afl-3.0 agpl-3.0 apache-2.0 artistic-2.0 blueoak-1.0.0 \
  bsd-2-clause bsd-2-clause-patent bsd-3-clause bsd-3-clause-clear bsd-4-clause \
  bsl-1.0 cc-by-4.0 cc-by-sa-4.0 cc0-1.0 cecill-2.1 cern-ohl-p-2.0 cern-ohl-s-2.0 \
  cern-ohl-w-2.0 ecl-2.0 epl-1.0 epl-2.0 eupl-1.1 eupl-1.2 gfdl-1.3 gpl-2.0 gpl-3.0 \
  isc lgpl-2.1 lgpl-3.0 lppl-1.3c mit mit-0 mpl-2.0 ms-pl ms-rl mulanpsl-2.0 ncsa \
  odbl-1.0 ofl-1.1 osl-3.0 postgresql unlicense upl-1.0 vim wtfpl zlib; do
  curl -fsS "$RAW/github/choosealicense.com/gh-pages/_licenses/$key.txt" -o "$LIC_DIR/$key.txt" \
    || echo "skip license $key" >&2
done

GI_DIR=crates/bgh-repos/data/gitignore
mkdir -p "$GI_DIR"
for name in $(cat "$GI_DIR/../gitignore-names.txt"); do
  curl -fsS "$RAW/github/gitignore/main/$name.gitignore" -o "$GI_DIR/$name.gitignore" \
    || echo "skip gitignore $name" >&2
done
