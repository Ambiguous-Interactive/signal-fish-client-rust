#!/usr/bin/env bash
# Shared credential-safe MCP launcher used by every supported agent frontend.

set -euo pipefail

env_loaded=false
if [ "${1:-}" = "--env-loaded" ]; then
    env_loaded=true
    shift
fi
server="${1:-}"

# Extension hosts need not load the interactive shell's PATH.
export PATH="${PATH}:/home/vscode/.local/bin:/usr/local/bin"

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'signal-fish-mcp: missing %s; run Dev Containers: Rebuild Container, then restart the agent inside the container\n' "$1" >&2
        return 1
    fi
}

# Prefer the workspace supplied by Dev Containers; CLIs started in a nested
# directory can instead resolve the repository root through git.
if [ "$env_loaded" = false ] && [ "$server" != "--check" ]; then
    workspace="${CONTAINER_WORKSPACE_FOLDER:-}"
    if [ -z "$workspace" ]; then
        workspace=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
    fi
    credential_file="${workspace%/}/.env.local"
    if [ -f "$credential_file" ]; then
        require_command node
        launcher_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
        exec node "$launcher_dir/mcp-env.cjs" "$credential_file" "$0" "$@"
    fi
fi

check_installation() {
    local status=0 command_name
    for command_name in signal-fish-mcp node zai-mcp-server mcp-remote github-mcp-server; do
        require_command "$command_name" || status=1
    done
    if [ "$status" -eq 0 ]; then
        printf 'signal-fish-mcp: all MCP executables are available (credentials and remote connections not checked)\n' >&2
    fi
    return "$status"
}

launch_github() {
    require_command github-mcp-server
    github_token="${GITHUB_PERSONAL_ACCESS_TOKEN:-${GITHUB_TOKEN:-${GH_TOKEN:-${GITHUB_PAT:-}}}}"
    if [ -z "$github_token" ] && command -v git >/dev/null 2>&1; then
        credential_result=$(printf 'protocol=https\nhost=github.com\n\n' |
            GIT_TERMINAL_PROMPT=0 git credential fill 2>/dev/null || true)
        github_token=$(printf '%s\n' "$credential_result" |
            sed -n 's/^password=//p' | tr -d '\r\n')
        unset credential_result
    fi
    if [ -n "$github_token" ]; then
        export GITHUB_PERSONAL_ACCESS_TOKEN="$github_token"
    fi
    unset github_token
    exec github-mcp-server stdio
}

launch_zai_remote() {
    local endpoint="$1"
    local runtime_dir header_file status
    require_command node
    require_command mcp-remote
    if [ -z "${Z_AI_API_KEY:-}" ]; then
        printf 'signal-fish-mcp: Z_AI_API_KEY is required for Z.AI MCP servers\n' >&2
        return 1
    fi
    runtime_dir="${XDG_RUNTIME_DIR:-/tmp}"
    if [ ! -d "$runtime_dir" ] || [ ! -w "$runtime_dir" ]; then
        runtime_dir=/tmp
    fi
    header_file=$(mktemp "${runtime_dir%/}/signal-fish-zai-header.XXXXXX")
    cleanup_header() {
        rm -f "$header_file"
    }
    trap cleanup_header EXIT HUP INT TERM
    chmod 600 "$header_file"
    printf 'Authorization: Bearer %s\n' "$Z_AI_API_KEY" > "$header_file"
    status=0
    mcp-remote "$endpoint" --header-file "$header_file" || status=$?
    cleanup_header
    trap - EXIT HUP INT TERM
    return "$status"
}

case "$server" in
    --check)
        check_installation
        ;;
    github)
        launch_github
        ;;
    zai-vision)
        require_command node
        require_command zai-mcp-server
        if [ -z "${Z_AI_API_KEY:-}" ]; then
            printf 'signal-fish-mcp: Z_AI_API_KEY is required for Z.AI MCP servers\n' >&2
            exit 1
        fi
        export Z_AI_MODE="${Z_AI_MODE:-ZAI}"
        exec zai-mcp-server
        ;;
    zai-web-search)
        launch_zai_remote "https://api.z.ai/api/mcp/web_search_prime/mcp"
        ;;
    zai-web-reader)
        launch_zai_remote "https://api.z.ai/api/mcp/web_reader/mcp"
        ;;
    zai-zread)
        launch_zai_remote "https://api.z.ai/api/mcp/zread/mcp"
        ;;
    *)
        printf 'usage: signal-fish-mcp {--check|github|zai-vision|zai-web-search|zai-web-reader|zai-zread}\n' >&2
        exit 2
        ;;
esac
