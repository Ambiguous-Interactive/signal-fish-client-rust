#!/usr/bin/env bash
# check-devcontainer-compat.sh — Validates that .devcontainer/devcontainer.json
# is cross-platform compatible for VS Code Dev Containers.
#
# The devcontainer spec runs initializeCommand on the HOST machine:
#   - On Windows: cmd.exe /c <string>   (or direct exec for arrays)
#   - On Linux/macOS: sh -c <string>    (or direct exec for arrays)
#
# A command using only Unix shell syntax (mkdir -p, touch, 2>/dev/null, etc.)
# will fail on Windows hosts.  This check enforces that initializeCommand either:
#   (a) uses the '||' fallback pattern to call PowerShell first then sh as fallback, OR
#   (b) is an array form with a cross-platform executable (pwsh, node, python3, etc.), OR
#   (c) is an object form where every named command is cross-platform.
#
# It also rejects required bind mounts from host-home credential paths
# (~/.ssh, ~/.gitconfig, ~/.gnupg). Those mounts are fragile because the source
# path must exist and be shared with Docker before the container can start, and
# variables like HOME are not guaranteed on Windows. VS Code already handles Git
# config copying and SSH agent forwarding; keep personal credential mounts in
# local, uncommitted overrides.
#
# IMPORTANT: The '||' fallback pattern responds to any non-zero exit code, not
# only 'binary not found'.  If powershell.exe runs but fails at runtime, the '||'
# will still trigger the sh fallback.  The PS1 script must therefore emit a clear
# diagnostic message before exiting non-zero so any runtime failure is visible.
#
# Exit codes:
#   0 — devcontainer config is cross-platform compatible
#   1 — one or more cross-platform compatibility violations were found

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DEVCONTAINER_JSON="$REPO_ROOT/.devcontainer/devcontainer.json"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

VIOLATIONS=0

# ─────────────────────────────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────────────────────────────

echo -e "${YELLOW}=== Devcontainer cross-platform compatibility check ===${NC}"
echo ""

if [ ! -f "$DEVCONTAINER_JSON" ]; then
    echo -e "${YELLOW}SKIP: .devcontainer/devcontainer.json not found — nothing to check.${NC}"
    exit 0
fi

if ! command -v python3 &>/dev/null; then
    # python3 is expected in all CI environments and the devcontainer itself.
    # Exit non-zero rather than silently skipping to avoid masking the check.
    echo -e "${RED}ERROR: python3 is required to parse JSONC but is not available.${NC}" >&2
    echo "       Install python3 or ensure it is on PATH." >&2
    exit 1
fi

# ─────────────────────────────────────────────────────────────────────────────
# Parse devcontainer.json (JSONC — strip comments before JSON-parsing)
# ─────────────────────────────────────────────────────────────────────────────

# Returns the initializeCommand value as:
#   STRING:<value>
#   ARRAY:<json-encoded-array>
#   OBJECT:<json-encoded-object>
#   MISSING:
INIT_CMD_INFO=$(python3 - "$DEVCONTAINER_JSON" <<'PYEOF'
import sys
import json

def strip_jsonc(s):
    """Strip // line comments and /* block comments from JSONC, preserving strings."""
    result = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == '"':
            # String literal — copy verbatim, handling escape sequences
            result.append(c)
            i += 1
            while i < n:
                c2 = s[i]
                result.append(c2)
                i += 1
                if c2 == '\\':
                    # Escaped character — copy next char too
                    if i < n:
                        result.append(s[i])
                        i += 1
                elif c2 == '"':
                    break
        elif s[i:i+2] == '//':
            # Line comment — skip to end of line
            while i < n and s[i] != '\n':
                i += 1
        elif s[i:i+2] == '/*':
            # Block comment — skip to */
            i += 2
            while i < n - 1 and s[i:i+2] != '*/':
                i += 1
            i += 2
        else:
            result.append(c)
            i += 1
    return ''.join(result)

def strip_trailing_commas(s):
    """Strip JSONC trailing commas before ] or }, preserving strings."""
    result = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == '"':
            result.append(c)
            i += 1
            while i < n:
                c2 = s[i]
                result.append(c2)
                i += 1
                if c2 == '\\':
                    if i < n:
                        result.append(s[i])
                        i += 1
                elif c2 == '"':
                    break
        elif c == ',':
            j = i + 1
            while j < n and s[j] in ' \t\r\n':
                j += 1
            if j < n and s[j] in '}]':
                i += 1
                continue
            result.append(c)
            i += 1
        else:
            result.append(c)
            i += 1
    return ''.join(result)

with open(sys.argv[1], encoding='utf-8') as f:
    content = f.read()

try:
    data = json.loads(strip_trailing_commas(strip_jsonc(content)))
except json.JSONDecodeError as e:
    print(f"PARSE_ERROR:{e}", file=sys.stderr)
    sys.exit(1)

cmd = data.get('initializeCommand')
if cmd is None:
    print('MISSING:')
elif isinstance(cmd, str):
    print(f'STRING:{cmd}')
elif isinstance(cmd, list):
    print(f'ARRAY:{json.dumps(cmd)}')
elif isinstance(cmd, dict):
    print(f'OBJECT:{json.dumps(cmd)}')
else:
    print(f'UNKNOWN:{type(cmd).__name__}')
PYEOF
) || {
    echo -e "${RED}FAIL: Could not parse .devcontainer/devcontainer.json${NC}"
    exit 1
}

FORM="${INIT_CMD_INFO%%:*}"
VALUE="${INIT_CMD_INFO#*:}"

echo "initializeCommand form: $FORM"

# ─────────────────────────────────────────────────────────────────────────────
# Helper: check a single string command for Unix-only patterns
# Returns 0 (no violations) or 1 (violation found)
# ─────────────────────────────────────────────────────────────────────────────
_check_string_command() {
    local cmd="$1"
    local label="${2:-initializeCommand}"

    local unix_shell_pattern='((/usr/bin/)?env[[:space:]]+(-S[[:space:]]+)?(bash|sh|zsh|fish|dash)|(/[^[:space:]]*/)?(bash|sh|zsh|fish|dash))'
    local windows_shell_pattern='((powershell|powershell\.exe|pwsh|pwsh\.exe|cmd|cmd\.exe))'

    local has_unix_fallback=false
    if printf '%s\n' "$cmd" | grep -qiE "\|\|[[:space:]]*${unix_shell_pattern}([[:space:]]|$)"; then
        has_unix_fallback=true
    fi

    # Windows-only shell executables fail on Linux/macOS unless there is an
    # explicit Unix fallback. This is the mirror image of the Unix-shell check
    # below and catches bare `powershell ...` / `cmd ...` host commands.
    if printf '%s\n' "$cmd" | grep -qiE "^[[:space:]]*${windows_shell_pattern}([[:space:]]|$)"; then
        if [ "$has_unix_fallback" = false ]; then
            echo -e "${RED}FAIL${NC}: $label starts with a Windows-only shell without a Unix fallback."
            echo "  Command: $cmd"
            echo ""
            echo "  Fix: Use the cross-platform '||' fallback pattern:"
            echo "    powershell -NoProfile -ExecutionPolicy Bypass -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"
            return 1
        fi
        return 0
    fi

    # Patterns that are UNIX-ONLY (will fail in Windows cmd.exe):
    #   mkdir -p         → Windows mkdir has no -p flag
    #   2>/dev/null      → Windows uses 2>nul
    #   ; touch <path>   → touch does not exist in Windows cmd.exe
    #   ^touch <path>    → touch at start of command
    #   bash/sh/...      → Unix shells are not guaranteed on Windows hosts
    #   || true          → 'true' is not a Windows cmd.exe builtin.
    #                      Use [^a-zA-Z_] after 'true' to match '|| true;' (with
    #                      semicolon) as well as '|| true' at end-of-string or
    #                      followed by whitespace.  This avoids false positives on
    #                      substrings like 'truecolor' or 'true-value'.
    local found_unix_pattern=false
    local unix_reason=""

    if printf '%s\n' "$cmd" | grep -qE "^[[:space:]]*${unix_shell_pattern}([[:space:]]|$)"; then
        found_unix_pattern=true
        unix_reason="starts with Unix-only shell executable"
    elif printf '%s\n' "$cmd" | grep -qE 'mkdir[[:space:]]+-p'; then
        found_unix_pattern=true
        unix_reason="mkdir -p (no -p flag in Windows cmd.exe)"
    elif printf '%s\n' "$cmd" | grep -qE '2>/dev/null'; then
        found_unix_pattern=true
        unix_reason="2>/dev/null (Windows uses 2>nul)"
    elif printf '%s\n' "$cmd" | grep -qE '(^|;[[:space:]]*)touch[[:space:]]'; then
        found_unix_pattern=true
        unix_reason="touch (not available in Windows cmd.exe)"
    elif printf '%s\n' "$cmd" | grep -qE '\|\|[[:space:]]*true([^a-zA-Z_]|$)'; then
        found_unix_pattern=true
        unix_reason="|| true (true is not a Windows cmd.exe builtin)"
    fi

    if [ "$found_unix_pattern" = false ]; then
        return 0
    fi

    # Unix pattern found.  Now check whether there is a Windows-compatible
    # fallback — i.e., the command uses the '||' fallback pattern to invoke
    # powershell/pwsh as the primary, with sh/bash as the Unix fallback.
    local has_windows_fallback=false

    # Pattern: starts with powershell or pwsh (Windows-primary)
    if printf '%s\n' "$cmd" | grep -qiE '^[[:space:]]*(powershell|pwsh)[[:space:]]'; then
        has_windows_fallback=true
    fi

    # Pattern: uses || to fall back to powershell/pwsh (Windows-as-fallback)
    if printf '%s\n' "$cmd" | grep -qiE '\|\|[[:space:]]*(powershell|pwsh)[[:space:]]'; then
        has_windows_fallback=true
    fi

    if [ "$has_windows_fallback" = true ]; then
        return 0
    fi

    echo -e "${RED}FAIL${NC}: $label uses Unix-only syntax without a Windows fallback."
    echo "  Command: $cmd"
    echo "  Reason:  $unix_reason"
    echo ""
    echo "  Fix: Use the cross-platform '||' fallback pattern:"
    echo "    powershell -NoProfile -ExecutionPolicy Bypass -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"
    echo ""
    echo "  How it works:"
    echo "    - Windows (cmd.exe): powershell succeeds → '||' short-circuits → sh not called"
    echo "    - Linux/macOS (sh):  powershell not found → exit 127 → '||' triggers → sh runs"
    echo "  Note: '||' responds to any non-zero exit, not just 'binary not found'."
    echo "  The PS1 script should emit diagnostics before exiting non-zero."
    return 1
}

# ─────────────────────────────────────────────────────────────────────────────
# Helper: check whether a string looks like a Unix-only shell executable
# Returns 0 if the executable is Unix-only, 1 otherwise
# ─────────────────────────────────────────────────────────────────────────────
_is_unix_only_executable() {
    local exe="$1"
    printf '%s\n' "$exe" | grep -qE '^(bash|sh|zsh|fish|dash)$'
}

# ─────────────────────────────────────────────────────────────────────────────
# Helper: check a command value that may be a string or an array (OBJECT form)
# Emits violations; returns 0 (ok) or 1 (violation).
# ─────────────────────────────────────────────────────────────────────────────
_check_object_value() {
    local raw_value="$1"   # JSON-encoded value (string or array)
    local key="$2"

    local form
    form=$(python3 -c "
import sys, json
v = json.loads(sys.argv[1])
if isinstance(v, str):
    print('STRING:' + v)
elif isinstance(v, list):
    print('ARRAY:' + json.dumps(v))
else:
    print('UNKNOWN:')
" "$raw_value" 2>/dev/null || true)

    local val_form="${form%%:*}"
    local val_value="${form#*:}"

    case "$val_form" in
        STRING)
            if ! _check_string_command "$val_value" "initializeCommand.$key"; then
                return 1
            fi
            ;;
        ARRAY)
            local exec_name
            exec_name=$(python3 -c "
import sys, json
arr = json.loads(sys.argv[1])
print(arr[0] if arr else '')
" "$val_value" 2>/dev/null || true)
            echo "  Object key '$key': array form — executable: $exec_name"
            if _is_unix_only_executable "$exec_name"; then
                echo -e "  ${RED}FAIL${NC}: initializeCommand.$key (array) uses Unix-only shell '$exec_name'."
                echo "  This will fail on Windows hosts."
                return 1
            fi
            ;;
        *)
            echo -e "  ${YELLOW}WARN${NC}: initializeCommand.$key has unexpected form '$val_form' — skipping."
            ;;
    esac
    return 0
}

# ─────────────────────────────────────────────────────────────────────────────
# Check initializeCommand based on its form
# ─────────────────────────────────────────────────────────────────────────────

case "$FORM" in
    MISSING)
        echo "  No initializeCommand defined — nothing to check."
        echo -e "${GREEN}PASS: No initializeCommand to validate.${NC}"
        ;;

    STRING)
        if _check_string_command "$VALUE" "initializeCommand (string)"; then
            echo -e "${GREEN}PASS: initializeCommand is cross-platform compatible.${NC}"
        else
            VIOLATIONS=$((VIOLATIONS + 1))
        fi
        ;;

    ARRAY)
        # Array form: runs executable directly (no shell).
        # Extract the executable name (first element).
        EXEC=$(python3 -c "
import sys, json
arr = json.loads(sys.argv[1])
print(arr[0] if arr else '')
" "$VALUE" 2>/dev/null || true)

        echo "  Array form — executable: $EXEC"

        # Array form with a clearly Unix-only executable is a violation.
        # Cross-platform executables: pwsh, node, python3, python, etc.
        # Unix-only executables: bash, sh, zsh, fish, etc.
        if _is_unix_only_executable "$EXEC"; then
            echo -e "${RED}FAIL${NC}: initializeCommand (array) uses Unix-only shell '$EXEC'."
            echo "  This will fail on Windows hosts where '$EXEC' is not guaranteed."
            echo ""
            echo "  Fix: Use a cross-platform executable (pwsh, node, python3) or"
            echo "  use the string form with the '||' fallback pattern:"
            echo "    powershell -NoProfile -ExecutionPolicy Bypass -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"
            VIOLATIONS=$((VIOLATIONS + 1))
        else
            echo -e "${GREEN}PASS: initializeCommand (array) uses cross-platform executable.${NC}"
        fi
        ;;

    OBJECT)
        # Object form: each key-value pair is a named parallel command.
        # Values may be strings or arrays — check both.
        echo "  Object form — checking each named command..."
        OBJECT_VIOLATIONS=0
        while IFS= read -r kv; do
            [ -z "$kv" ] && continue
            KEY="${kv%%=*}"
            CMD_JSON="${kv#*=}"
            if ! _check_object_value "$CMD_JSON" "$KEY"; then
                OBJECT_VIOLATIONS=$((OBJECT_VIOLATIONS + 1))
            fi
        done < <(python3 -c "
import sys, json
obj = json.loads(sys.argv[1])
for k, v in obj.items():
    # Emit key=<JSON-encoded value> so the shell can split on first '='
    print(k + '=' + json.dumps(v))
" "$VALUE" 2>/dev/null || true)

        if [ "$OBJECT_VIOLATIONS" -eq 0 ]; then
            echo -e "${GREEN}PASS: All named commands in initializeCommand (object) are cross-platform.${NC}"
        else
            VIOLATIONS=$((VIOLATIONS + OBJECT_VIOLATIONS))
        fi
        ;;

    *)
        echo -e "${YELLOW}SKIP: Unknown initializeCommand form '$FORM' — cannot validate.${NC}"
        ;;
esac

echo ""

# ─────────────────────────────────────────────────────────────────────────────
# Check mounts for required host-home credential bind mounts
# ─────────────────────────────────────────────────────────────────────────────

MOUNT_INFO=$(python3 - "$DEVCONTAINER_JSON" <<'PYEOF'
import sys
import json

def strip_jsonc(s):
    result = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == '"':
            result.append(c)
            i += 1
            while i < n:
                c2 = s[i]
                result.append(c2)
                i += 1
                if c2 == '\\':
                    if i < n:
                        result.append(s[i])
                        i += 1
                elif c2 == '"':
                    break
        elif s[i:i+2] == '//':
            while i < n and s[i] != '\n':
                i += 1
        elif s[i:i+2] == '/*':
            i += 2
            while i < n - 1 and s[i:i+2] != '*/':
                i += 1
            i += 2
        else:
            result.append(c)
            i += 1
    return ''.join(result)

def strip_trailing_commas(s):
    result = []
    i = 0
    n = len(s)
    while i < n:
        c = s[i]
        if c == '"':
            result.append(c)
            i += 1
            while i < n:
                c2 = s[i]
                result.append(c2)
                i += 1
                if c2 == '\\':
                    if i < n:
                        result.append(s[i])
                        i += 1
                elif c2 == '"':
                    break
        elif c == ',':
            j = i + 1
            while j < n and s[j] in ' \t\r\n':
                j += 1
            if j < n and s[j] in '}]':
                i += 1
                continue
            result.append(c)
            i += 1
        else:
            result.append(c)
            i += 1
    return ''.join(result)

def parse_mount_string(raw):
    fields = {}
    for part in raw.split(','):
        if '=' in part:
            key, value = part.split('=', 1)
            fields[key.strip()] = value.strip()
        else:
            fields[part.strip()] = True
    return fields

with open(sys.argv[1], encoding='utf-8') as f:
    data = json.loads(strip_trailing_commas(strip_jsonc(f.read())))

for index, mount in enumerate(data.get('mounts') or []):
    if isinstance(mount, str):
        fields = parse_mount_string(mount)
        form = 'string'
        raw = mount
    elif isinstance(mount, dict):
        fields = mount
        form = 'object'
        raw = json.dumps(mount, sort_keys=True)
    else:
        fields = {}
        form = type(mount).__name__
        raw = json.dumps(mount)

    source = fields.get('source') or fields.get('src') or ''
    target = fields.get('target') or fields.get('dst') or fields.get('destination') or ''
    mount_type = fields.get('type') or ''
    print(f'{index}\t{form}\t{mount_type}\t{source}\t{target}\t{raw}')
PYEOF
) || {
    echo -e "${RED}FAIL: Could not parse mounts in .devcontainer/devcontainer.json${NC}"
    exit 1
}

echo "Checking mounts for required host-home credential binds..."

MOUNT_VIOLATIONS=0
if [ -z "$MOUNT_INFO" ]; then
    echo -e "${GREEN}PASS: No extra mounts defined.${NC}"
else
    while IFS=$'\t' read -r index form mount_type source target raw; do
        [ -n "${index:-}" ] || continue

        if [ "$mount_type" != "bind" ]; then
            continue
        fi

        forbidden_reason=""
        case "$source" in
            *'${localEnv:HOME}'*|*'${localEnv:USERPROFILE}'*|'~'|'~/'*|\
            *'$HOME'*|*'%USERPROFILE%'*)
                forbidden_reason="required bind mount uses a host-home environment variable or ~"
                ;;
        esac

        case "$source" in
            *'/.ssh'|*'/.ssh/'*|*'\\.ssh'|*'\\.ssh\\'*|\
            *'/.gnupg'|*'/.gnupg/'*|*'\\.gnupg'|*'\\.gnupg\\'*|\
            *'/.gitconfig'|*'\\.gitconfig')
                forbidden_reason="required bind mount reads host credential/config path"
                ;;
        esac

        case "$target" in
            '/home/vscode/.ssh'|'/home/vscode/.ssh/'*|'/root/.ssh'|'/root/.ssh/'*|\
            '/home/vscode/.gnupg'|'/home/vscode/.gnupg/'*|'/root/.gnupg'|'/root/.gnupg/'*|\
            '/home/vscode/.gitconfig'|'/root/.gitconfig')
                forbidden_reason="required bind mount targets container credential/config path"
                ;;
        esac

        if [ -n "$forbidden_reason" ]; then
            echo -e "${RED}FAIL${NC}: mounts[$index] is not cross-platform reliable."
            echo "  Mount:  $raw"
            echo "  Source: $source"
            echo "  Target: $target"
            echo "  Reason: $forbidden_reason"
            echo ""
            echo "  Fix: Do not commit required ~/.ssh, ~/.gitconfig, or ~/.gnupg bind mounts."
            echo "       VS Code copies Git config and forwards the SSH agent automatically."
            echo "       Keep personal credential mounts in local, uncommitted overrides."
            MOUNT_VIOLATIONS=$((MOUNT_VIOLATIONS + 1))
        fi
    done <<< "$MOUNT_INFO"

    if [ "$MOUNT_VIOLATIONS" -eq 0 ]; then
        echo -e "${GREEN}PASS: No required host-home credential bind mounts found.${NC}"
    else
        VIOLATIONS=$((VIOLATIONS + MOUNT_VIOLATIONS))
    fi
fi

echo ""

# ─────────────────────────────────────────────────────────────────────────────
# Check referenced initialize-host scripts exist
# ─────────────────────────────────────────────────────────────────────────────

PS1_SCRIPT="$REPO_ROOT/.devcontainer/scripts/initialize-host.ps1"
SH_SCRIPT="$REPO_ROOT/.devcontainer/scripts/initialize-host.sh"

if printf '%s\n' "$VALUE" | grep -qF '.devcontainer/scripts/initialize-host.ps1'; then
    if [ -f "$PS1_SCRIPT" ]; then
        echo -e "${GREEN}PASS${NC}: referenced initialize-host.ps1 exists."
    else
        echo -e "${RED}FAIL${NC}: initializeCommand references .devcontainer/scripts/initialize-host.ps1 but it does not exist."
        VIOLATIONS=$((VIOLATIONS + 1))
    fi
fi

if printf '%s\n' "$VALUE" | grep -qF '.devcontainer/scripts/initialize-host.sh'; then
    if [ -f "$SH_SCRIPT" ]; then
        echo -e "${GREEN}PASS${NC}: referenced initialize-host.sh exists."
    else
        echo -e "${RED}FAIL${NC}: initializeCommand references .devcontainer/scripts/initialize-host.sh but it does not exist."
        VIOLATIONS=$((VIOLATIONS + 1))
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# Final result
# ─────────────────────────────────────────────────────────────────────────────

if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}=== FAIL: $VIOLATIONS cross-platform compatibility violation(s) found. ===${NC}"
    exit 1
else
    echo -e "${GREEN}=== PASS: devcontainer cross-platform compatibility check passed. ===${NC}"
    exit 0
fi
