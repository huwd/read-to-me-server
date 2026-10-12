#!/usr/bin/env bash
# Vendor misaki-rs 0.6.0 with the upstream Misaki lexicons (hexgrad/misaki, Apache-2.0)
# in place of its espeak-generated ones. Spike only (issue #17); output is git-ignored.
set -euo pipefail
cd "$(dirname "$0")"
src=$(ls -d "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/misaki-rs-0.6.0 | head -1)
upstream=$(reference/.venv/bin/python -c 'import misaki, os; print(os.path.join(os.path.dirname(misaki.__file__), "data"))')
rm -rf vendor/misaki-rs && mkdir -p vendor && cp -r "$src" vendor/misaki-rs
for f in gb_gold gb_silver us_gold us_silver; do cp "$upstream/$f.json" "vendor/misaki-rs/data/$f.json"; done
for p in misaki-rs-*.patch; do patch -d vendor/misaki-rs -p1 --quiet < "$p"; done
echo "vendored misaki-rs with upstream lexicons from $upstream (+ spike patches)"
