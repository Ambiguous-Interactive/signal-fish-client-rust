#!/usr/bin/env bash
# =============================================================================
# Signal Fish Client SDK - Post-Start Hook
# =============================================================================
# Runs every time the container starts (not just on first create).
# Kept separate from devcontainer.json to avoid JSON escape complexity and
# to make debugging easier.
#
# IMPORTANT: This script runs as the 'vscode' user with sudo available.
# =============================================================================

set -euo pipefail

# -----------------------------------------------------------------------------
# Git safe.directory (system-level default for this container)
# -----------------------------------------------------------------------------
# safe.directory uses --add because it is a multi-valued git key.
# --replace-all would destroy any other safe.directory entries set by
# other tools or previous lifecycle hook runs.
WORKSPACE_FOLDER="${CONTAINER_WORKSPACE_FOLDER:-/workspaces/signal-fish-client}"
if sudo git config --system --get-all safe.directory 2>/dev/null | grep -qxF "${WORKSPACE_FOLDER}"; then
    echo "post-start: safe.directory already configured for ${WORKSPACE_FOLDER}"
elif sudo git config --system --add safe.directory "${WORKSPACE_FOLDER}" 2>/dev/null; then
    echo "post-start: safe.directory configured for ${WORKSPACE_FOLDER}"
else
    echo "post-start: WARNING: safe.directory configuration failed (git may show 'dubious ownership')"
fi

# -----------------------------------------------------------------------------
# Delta theme (light mode support)
# -----------------------------------------------------------------------------
if [ "${COLORSCHEME:-}" = "light" ]; then
    if sudo git config --system delta.light true 2>/dev/null; then
        echo "post-start: delta light mode enabled"
    else
        echo "post-start: WARNING: delta light mode configuration failed"
    fi
fi

echo "post-start: done"
