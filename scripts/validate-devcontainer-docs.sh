#!/usr/bin/env bash
# =============================================================================
# validate-devcontainer-docs.sh
#
# Validates that every devcontainer lifecycle hook mentioned in
# .devcontainer/README.md actually exists as a key in .devcontainer/devcontainer.json.
#
# This catches documentation drift — e.g., the README referring to
# "updateContentCommand" when the config only defines "postCreateCommand".
#
# Exit codes:
#   0 — all hooks mentioned in the README exist in devcontainer.json
#   1 — one or more hooks are documented but missing from the config
# =============================================================================

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

README="$REPO_ROOT/.devcontainer/README.md"
CONFIG="$REPO_ROOT/.devcontainer/devcontainer.json"

# All devcontainer lifecycle hooks (per the dev container spec)
LIFECYCLE_HOOKS=(
    initializeCommand
    onCreateCommand
    updateContentCommand
    postCreateCommand
    postStartCommand
    postAttachCommand
)

# ---- Preflight checks -------------------------------------------------------

if [[ ! -f "$README" ]]; then
    echo "ERROR: README not found at $README"
    exit 1
fi

if [[ ! -f "$CONFIG" ]]; then
    echo "ERROR: devcontainer.json not found at $CONFIG"
    exit 1
fi

# ---- Validation --------------------------------------------------------------

errors=0

for hook in "${LIFECYCLE_HOOKS[@]}"; do
    # Check if the hook name appears in the README (as a word, case-sensitive)
    if grep -qw "$hook" "$README"; then
        # Hook is mentioned in docs — verify it exists as a key in devcontainer.json.
        # We look for the hook name as a JSON key (quoted, at the start of a
        # key-value pair). Using grep on the raw JSONC is intentional: jq cannot
        # parse JSONC comments.
        if ! grep -qE "^[[:space:]]*\"${hook}\"[[:space:]]*:" "$CONFIG"; then
            echo "MISMATCH: '$hook' is documented in README.md but is NOT a key in devcontainer.json"
            errors=$((errors + 1))
        fi
    fi
done

# ---- Result ------------------------------------------------------------------

if [[ "$errors" -gt 0 ]]; then
    echo ""
    echo "FAILED: $errors lifecycle hook(s) documented in README.md but missing from devcontainer.json"
    exit 1
else
    echo "OK: all lifecycle hooks mentioned in README.md exist in devcontainer.json"
    exit 0
fi
