#!/usr/bin/env bash
# Install or refresh the user-owned agent CLI toolchain.

set -euo pipefail

MODE="${1:-build}"
PACKAGES=(
    "@openai/codex@latest"
    "opencode-ai@latest"
    "@nanocollective/nanocoder@latest"
    "@anthropic-ai/claude-code@latest"
    "@github/copilot@latest"
    "@z_ai/mcp-server@latest"
    "mcp-remote@latest"
)
COMMANDS=(codex opencode nanocoder claude copilot zai-mcp-server mcp-remote)

warn_or_fail() {
    local message="$1"
    if [ "$MODE" = "--update" ]; then
        printf 'agent-tools: WARNING: %s; continuing with the image-installed toolchain\n' "$message" >&2
        return 0
    fi
    printf 'agent-tools: ERROR: %s\n' "$message" >&2
    return 1
}

node_major=$(node --version 2>/dev/null | sed -nE 's/^v([0-9]+).*/\1/p')
if [ -z "$node_major" ] || [ "$node_major" -lt 22 ]; then
    warn_or_fail "Node.js 22 or newer is required" || exit 1
    exit 0
fi

npm_prefix=$(npm config get prefix 2>/dev/null || true)
if [ "$npm_prefix" != "/home/vscode/.local" ] || [ ! -w "$npm_prefix" ]; then
    warn_or_fail "npm's global prefix must be the writable /home/vscode/.local" || exit 1
    exit 0
fi

install_specs=()
if [ "$MODE" = "--update" ]; then
    version_dir=$(mktemp -d)
    cleanup_versions() {
        rm -rf "$version_dir"
    }
    trap cleanup_versions EXIT HUP INT TERM

    pids=()
    for index in "${!PACKAGES[@]}"; do
        npm view "${PACKAGES[$index]}" version > "$version_dir/$index" 2>/dev/null &
        pids+=("$!")
    done

    version_lookup_failed=false
    for pid in "${pids[@]}"; do
        if ! wait "$pid"; then
            version_lookup_failed=true
        fi
    done
    if [ "$version_lookup_failed" = true ]; then
        warn_or_fail "the npm registry version check failed"
        exit 0
    fi

    installed_json=$(npm list --global --depth=0 --json 2>/dev/null || true)
    for index in "${!PACKAGES[@]}"; do
        package="${PACKAGES[$index]%@latest}"
        latest=$(tr -d '\r\n' < "$version_dir/$index")
        current=$(printf '%s' "$installed_json" | jq -r --arg package "$package" \
            '.dependencies[$package].version // empty' 2>/dev/null || true)
        if [ -z "$latest" ]; then
            warn_or_fail "the npm registry returned no version for $package"
            exit 0
        fi
        if [ "$current" != "$latest" ]; then
            install_specs+=("${PACKAGES[$index]}")
        fi
    done
else
    install_specs=("${PACKAGES[@]}")
fi

if [ ${#install_specs[@]} -gt 0 ]; then
    printf 'agent-tools: installing %s package(s) into %s\n' \
        "${#install_specs[@]}" "$npm_prefix"
    if ! npm install --global --no-audit --no-fund --loglevel=warn "${install_specs[@]}"; then
        warn_or_fail "npm could not install the latest agent tools" || exit 1
        exit 0
    fi
else
    printf 'agent-tools: all npm-installed agents and MCP bridges are current\n'
fi

missing_commands=()
for command_name in "${COMMANDS[@]}"; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        missing_commands+=("$command_name")
    fi
done
if [ ${#missing_commands[@]} -gt 0 ]; then
    warn_or_fail "installed commands are missing: ${missing_commands[*]}" || exit 1
fi
