#!/usr/bin/env bash
# =============================================================================
# Signal Fish Client SDK - Post-Start Hook
# =============================================================================
# Runs every time the container starts (not just on first create).
# Kept separate from devcontainer.json to avoid JSON escape complexity and
# to make debugging easier.
#
# IMPORTANT: This script runs as the unprivileged 'vscode' user.
# =============================================================================

set -euo pipefail

# Run the workspace copy so an old image can explain its missing toolchain.
# Refreshing npm packages cannot add binaries installed by Dockerfile COPY/RUN.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if ! bash "$SCRIPT_DIR/mcp-launch.sh" --check; then
    printf 'post-start: MCP setup is incomplete; rebuild this devcontainer before starting an agent\n' >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# Git safe.directory (system-level default for this container)
# -----------------------------------------------------------------------------
# safe.directory uses --add because it is a multi-valued git key.
# --replace-all would destroy any other safe.directory entries set by
# other tools or previous lifecycle hook runs.
WORKSPACE_FOLDER="${CONTAINER_WORKSPACE_FOLDER:-/workspaces/signal-fish-client}"
if git config --global --get-all safe.directory 2>/dev/null | grep -qxF "${WORKSPACE_FOLDER}"; then
    echo "post-start: safe.directory already configured for ${WORKSPACE_FOLDER}"
elif git config --global --add safe.directory "${WORKSPACE_FOLDER}" 2>/dev/null; then
    echo "post-start: safe.directory configured for ${WORKSPACE_FOLDER}"
else
    echo "post-start: WARNING: safe.directory configuration failed (git may show 'dubious ownership')"
fi

# -----------------------------------------------------------------------------
# Delta theme (light mode support)
# -----------------------------------------------------------------------------
if [ "${COLORSCHEME:-}" = "light" ]; then
    if git config --global delta.light true 2>/dev/null; then
        echo "post-start: delta light mode enabled"
    else
        echo "post-start: WARNING: delta light mode configuration failed"
    fi
fi

# -----------------------------------------------------------------------------
# Agent CLI freshness
# -----------------------------------------------------------------------------
# Image creation installs a complete known-working set. On later launches this
# performs parallel registry version lookups and invokes npm only when one of
# the tools has actually changed. A registry outage never prevents VS Code from
# attaching because the image-installed versions remain available.
if /usr/local/bin/install-agent-tools.sh --update; then
    echo "post-start: agent CLI versions checked"
else
    echo "post-start: WARNING: agent CLI refresh failed; using image-installed versions"
fi

# Nanocoder bundles its official VS Code extension in the npm package rather
# than publishing it separately. Install it once when the VS Code CLI is ready.
if command -v code >/dev/null 2>&1 \
    && ! code --list-extensions 2>/dev/null | grep -qi '^nanocollective\.nanocoder-vscode$'; then
    NANOCODER_VSIX="$(npm root --global)/@nanocollective/nanocoder/assets/nanocoder-vscode.vsix"
    if [ -f "$NANOCODER_VSIX" ]; then
        if code --install-extension "$NANOCODER_VSIX" >/dev/null 2>&1; then
            echo "post-start: Nanocoder VS Code extension installed"
        else
            echo "post-start: WARNING: Nanocoder VS Code extension installation failed"
        fi
    fi
fi

echo "post-start: done"
