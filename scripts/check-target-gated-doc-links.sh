#!/usr/bin/env bash
# check-target-gated-doc-links.sh — Detect intra-doc links to target-gated types.
#
# Types gated on target_os = "emscripten" (like EmscriptenWebSocketTransport)
# cannot be resolved by rustdoc on non-emscripten hosts. Source files OUTSIDE
# the emscripten module must use plain backticks (`TypeName`) instead of
# intra-doc links ([`TypeName`]).
#
# Checks:
#   1. No .rs file under src/ (excluding the emscripten_websocket module) may
#      contain [`EmscriptenWebSocketTransport (intra-doc link syntax).
#
# The pattern omits the trailing `]` to also catch method-level links like
# [`EmscriptenWebSocketTransport::connect`].
#
# Exit codes:
#   0 — no violations found
#   1 — one or more violations detected

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

VIOLATIONS=0

echo -e "${YELLOW}=== Target-gated doc-link check ===${NC}"
echo ""
echo -e "${YELLOW}Scanning for intra-doc links to EmscriptenWebSocketTransport in non-emscripten files...${NC}"

# The forbidden pattern: an intra-doc link opening bracket+backtick followed
# by the type name. We omit the closing `]` so we also catch method links
# like [`EmscriptenWebSocketTransport::connect`].
FORBIDDEN='[`EmscriptenWebSocketTransport'

# Walk all .rs files under src/.
while IFS= read -r -d '' file; do
    # Exclude files inside the emscripten_websocket module (target-gated).
    # Match the exact directory name "emscripten_websocket" or exact filename
    # "emscripten_websocket.rs" — NOT a prefix match — to avoid silently
    # skipping unrelated files that happen to share the prefix.
    skip=false
    remaining="${file#"$REPO_ROOT"/src/}"
    # Split on / and check each directory component.
    while [[ "$remaining" == */* ]]; do
        component="${remaining%%/*}"
        remaining="${remaining#*/}"
        if [[ "$component" == "emscripten_websocket" ]]; then
            skip=true
            break
        fi
    done
    # Check the final component (filename) for exact match.
    if [[ "$remaining" == "emscripten_websocket.rs" ]]; then
        skip=true
    fi

    if [ "$skip" = true ]; then
        continue
    fi

    # Scan the file line by line for the forbidden pattern.
    lineno=0
    while IFS= read -r line; do
        line="${line//$'\r'/}"
        lineno=$((lineno + 1))
        if [[ "$line" == *"$FORBIDDEN"* ]]; then
            echo -e "${RED}VIOLATION:${NC} $file:$lineno: intra-doc link to target-gated type"
            echo "  $line"
            echo "  This type is gated on target_os = \"emscripten\" and cannot be resolved"
            echo "  by rustdoc on other hosts. Use plain backtick formatting:"
            echo "    \`EmscriptenWebSocketTransport\` instead of [\`EmscriptenWebSocketTransport\`]"
            VIOLATIONS=$((VIOLATIONS + 1))
        fi
    done < "$file"
done < <(find src/ -name '*.rs' -print0)

echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS target-gated doc-link violation(s) found.${NC}"
    echo "Fix all violations before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No target-gated doc-link issues found.${NC}"
    exit 0
fi
