#!/usr/bin/env bash
# check-docsrs.sh — Simulate docs.rs rustdoc build locally/CI.
#
# docs.rs uses nightly rustdoc with `--cfg docsrs`. This script catches
# docs.rs-only breakage (for example removed nightly feature gates) before
# publishing a release.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

echo "==> docs.rs compatibility check"
echo "    repo: $REPO_ROOT"
echo "    rustc (stable): $(rustc --version)"
echo "    rustc (nightly): $(rustc +nightly --version)"

# Always include docs.rs cfg; keep warnings denied to match CI policy.
export RUSTDOCFLAGS="--cfg docsrs -D warnings"

echo "==> running: cargo +nightly doc --all-features --no-deps"
cargo +nightly doc --all-features --no-deps

echo "==> docs.rs compatibility check passed"
