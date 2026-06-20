#!/usr/bin/env bash
# test_check_devcontainer_compat.sh — Unit tests for check-devcontainer-compat.sh.
#
# Tests the detection logic by creating temporary devcontainer.json fixtures and
# verifying that the check script accepts or rejects them as expected.
#
# Usage:
#   bash scripts/test_check_devcontainer_compat.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CHECK_SCRIPT="$REPO_ROOT/scripts/check-devcontainer-compat.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m'

CHECKS_RUN=0
CHECKS_PASSED=0
CHECKS_FAILED=0

# ── Helpers ───────────────────────────────────────────────────────────────────

_pass() {
    echo -e "  ${GREEN}PASS${NC}: $1"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
    CHECKS_RUN=$((CHECKS_RUN + 1))
}

_fail() {
    echo -e "  ${RED}FAIL${NC}: $1"
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    CHECKS_RUN=$((CHECKS_RUN + 1))
}

# ── Fixture helpers ───────────────────────────────────────────────────────────

TMPDIR_FIXTURES="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_FIXTURES"' EXIT

# Create a minimal devcontainer.json fixture with the given initializeCommand
# value and optional mounts value. Values are embedded as raw JSON.
_make_fixture() {
    local dir="$1"
    local init_cmd_json="$2"          # raw JSON value (string, array, object, or null)
    local mounts_json="${3:-MISSING}" # raw JSON array or MISSING
    local fixture_dir="$dir/.devcontainer"
    mkdir -p "$fixture_dir/scripts"

    # Create placeholder scripts so tests that reference host initializer files
    # exercise command compatibility, not missing-script handling.
    touch "$fixture_dir/scripts/initialize-host.ps1"
    touch "$fixture_dir/scripts/initialize-host.sh"

    {
        printf '{\n'
        printf '  "name": "Test"'
        if [ "$init_cmd_json" != "MISSING" ]; then
            printf ',\n  "initializeCommand": %s' "$init_cmd_json"
        fi
        if [ "$mounts_json" != "MISSING" ]; then
            printf ',\n  "mounts": %s' "$mounts_json"
        fi
        printf '\n}\n'
    } > "$fixture_dir/devcontainer.json"
}

# Run check-devcontainer-compat.sh against a fixture directory.
# $1 — fixture root directory
# $2 — expected exit code (0 = PASS expected, 1 = FAIL expected)
# $3 — test description
_run_test() {
    local fixture_dir="$1"
    local expected_exit="$2"
    local description="$3"

    # Temporarily override REPO_ROOT by running the check from the fixture dir.
    # The check script uses "$(dirname $0)/.." for REPO_ROOT, so we symlink the
    # scripts/ directory into the fixture so REPO_ROOT resolves to the fixture.
    local fixture_scripts="$fixture_dir/scripts"
    mkdir -p "$fixture_scripts"

    # Symlink the real check script into the fixture's scripts/ dir
    ln -sf "$CHECK_SCRIPT" "$fixture_scripts/check-devcontainer-compat.sh"

    local actual_exit=0
    bash "$fixture_scripts/check-devcontainer-compat.sh" > /dev/null 2>&1 || actual_exit=$?

    if [ "$actual_exit" -eq "$expected_exit" ]; then
        _pass "$description"
    else
        _fail "$description (expected exit $expected_exit, got $actual_exit)"
    fi
}

# ── Tests ─────────────────────────────────────────────────────────────────────

echo -e "${BOLD}${YELLOW}=== Tests: check-devcontainer-compat.sh ===${NC}"
echo ""

# ── Group 1: Unix-only commands that MUST be rejected ─────────────────────────

echo -e "${YELLOW}Group 1: Unix-only initializeCommand values (must FAIL)${NC}"

# Current broken pattern (the original bug this check was written to catch)
D="$TMPDIR_FIXTURES/t01"; mkdir -p "$D"
_make_fixture "$D" '"mkdir -p ~/.ssh ~/.gnupg 2>/dev/null || true; touch ~/.gitconfig 2>/dev/null || true; echo done"'
_run_test "$D" 1 "Rejects: original broken command (mkdir -p + touch + 2>/dev/null)"

D="$TMPDIR_FIXTURES/t02"; mkdir -p "$D"
_make_fixture "$D" '"mkdir -p ~/.ssh"'
_run_test "$D" 1 "Rejects: bare mkdir -p"

D="$TMPDIR_FIXTURES/t03"; mkdir -p "$D"
_make_fixture "$D" '"touch ~/.gitconfig"'
_run_test "$D" 1 "Rejects: bare touch at start of command"

D="$TMPDIR_FIXTURES/t04"; mkdir -p "$D"
_make_fixture "$D" '"echo hi; touch ~/.gitconfig"'
_run_test "$D" 1 "Rejects: touch after semicolon"

D="$TMPDIR_FIXTURES/t05"; mkdir -p "$D"
_make_fixture "$D" '"ls 2>/dev/null"'
_run_test "$D" 1 "Rejects: 2>/dev/null redirect"

D="$TMPDIR_FIXTURES/t06"; mkdir -p "$D"
_make_fixture "$D" '"some-cmd || true"'
_run_test "$D" 1 "Rejects: || true (Unix-only exit guard)"

D="$TMPDIR_FIXTURES/t07"; mkdir -p "$D"
_make_fixture "$D" '["bash", "-c", "mkdir -p ~/.ssh"]'
_run_test "$D" 1 "Rejects: array form with bash executable"

D="$TMPDIR_FIXTURES/t08"; mkdir -p "$D"
_make_fixture "$D" '["sh", "-c", "touch ~/.gitconfig"]'
_run_test "$D" 1 "Rejects: array form with sh executable"

# || true followed by semicolon (was a regex gap in earlier versions)
D="$TMPDIR_FIXTURES/t09"; mkdir -p "$D"
_make_fixture "$D" '"some-cmd || true; echo done"'
_run_test "$D" 1 "Rejects: || true; (semicolon suffix — boundary pattern)"

D="$TMPDIR_FIXTURES/t09b"; mkdir -p "$D"
_make_fixture "$D" '"bash .devcontainer/scripts/initialize-host.sh"'
_run_test "$D" 1 "Rejects: string form starting with bash"

D="$TMPDIR_FIXTURES/t09c"; mkdir -p "$D"
_make_fixture "$D" '"sh -c \"echo ok\""'
_run_test "$D" 1 "Rejects: string form starting with sh -c"

D="$TMPDIR_FIXTURES/t09d"; mkdir -p "$D"
_make_fixture "$D" '"/bin/bash -c \"echo ok\""'
_run_test "$D" 1 "Rejects: string form starting with /bin/bash"

D="$TMPDIR_FIXTURES/t09e"; mkdir -p "$D"
_make_fixture "$D" '"/usr/bin/env bash .devcontainer/scripts/initialize-host.sh"'
_run_test "$D" 1 "Rejects: string form starting with /usr/bin/env bash"

D="$TMPDIR_FIXTURES/t09f"; mkdir -p "$D"
_make_fixture "$D" '"env sh .devcontainer/scripts/initialize-host.sh"'
_run_test "$D" 1 "Rejects: string form starting with env sh"

echo ""

# ── Group 2: Cross-platform commands that MUST be accepted ────────────────────

echo -e "${YELLOW}Group 2: Cross-platform initializeCommand values (must PASS)${NC}"

# The canonical fix pattern
D="$TMPDIR_FIXTURES/t10"; mkdir -p "$D"
_make_fixture "$D" '"powershell -NoProfile -ExecutionPolicy Bypass -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"'
_run_test "$D" 0 "Accepts: canonical powershell || sh fallback pattern"

D="$TMPDIR_FIXTURES/t11"; mkdir -p "$D"
_make_fixture "$D" '"pwsh -NoProfile -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"'
_run_test "$D" 0 "Accepts: pwsh (PowerShell Core) || sh fallback pattern"

D="$TMPDIR_FIXTURES/t12"; mkdir -p "$D"
_make_fixture "$D" '["pwsh", "-NoProfile", "-File", ".devcontainer/scripts/initialize-host.ps1"]'
_run_test "$D" 0 "Accepts: array form with pwsh (cross-platform)"

D="$TMPDIR_FIXTURES/t13"; mkdir -p "$D"
_make_fixture "$D" '["node", "scripts/setup.js"]'
_run_test "$D" 0 "Accepts: array form with node (cross-platform)"

D="$TMPDIR_FIXTURES/t14"; mkdir -p "$D"
_make_fixture "$D" '["python3", "scripts/setup.py"]'
_run_test "$D" 0 "Accepts: array form with python3 (cross-platform)"

D="$TMPDIR_FIXTURES/t15"; mkdir -p "$D"
_make_fixture "$D" '"echo hello"'
_run_test "$D" 0 "Accepts: simple echo (no Unix-specific patterns)"

# ── Group 3: Edge cases ───────────────────────────────────────────────────────

echo ""
echo -e "${YELLOW}Group 3: Edge cases${NC}"

D="$TMPDIR_FIXTURES/t20"; mkdir -p "$D"
_make_fixture "$D" "MISSING"
_run_test "$D" 0 "Accepts: no initializeCommand at all"

# Object form with both platforms (all cross-platform)
D="$TMPDIR_FIXTURES/t21"; mkdir -p "$D"
_make_fixture "$D" '{"setup": "echo hello"}'
_run_test "$D" 0 "Accepts: object form with cross-platform commands"

# Object form where one named command is Unix-only
D="$TMPDIR_FIXTURES/t22"; mkdir -p "$D"
_make_fixture "$D" '{"setup": "mkdir -p ~/.ssh"}'
_run_test "$D" 1 "Rejects: object form where named command uses mkdir -p"

# Object form where a named command is an array with Unix-only executable
D="$TMPDIR_FIXTURES/t24"; mkdir -p "$D"
_make_fixture "$D" '{"setup": ["bash", "setup.sh"]}'
_run_test "$D" 1 "Rejects: object form where named command is array with bash"

# Object form with cross-platform array command
D="$TMPDIR_FIXTURES/t25"; mkdir -p "$D"
_make_fixture "$D" '{"setup": ["pwsh", "-File", "setup.ps1"]}'
_run_test "$D" 0 "Accepts: object form where named command is array with pwsh"

D="$TMPDIR_FIXTURES/t23"; mkdir -p "$D"
_make_fixture "$D" '"powershell -NoProfile -Command \"Write-Host ok\""'
_run_test "$D" 1 "Rejects: powershell-primary without Unix fallback"

D="$TMPDIR_FIXTURES/t23b"; mkdir -p "$D"
_make_fixture "$D" '"pwsh -NoProfile -Command \"Write-Host ok\""'
_run_test "$D" 1 "Rejects: pwsh-primary without Unix fallback"

D="$TMPDIR_FIXTURES/t23c"; mkdir -p "$D"
_make_fixture "$D" '"cmd /c echo ok"'
_run_test "$D" 1 "Rejects: cmd-primary without Unix fallback"

D="$TMPDIR_FIXTURES/t26"; mkdir -p "$D"
_make_fixture "$D" '"powershell -NoProfile -ExecutionPolicy Bypass -File .devcontainer/scripts/initialize-host.ps1 || sh .devcontainer/scripts/initialize-host.sh"'
rm -f "$D/.devcontainer/scripts/initialize-host.ps1"
_run_test "$D" 1 "Rejects: referenced initializer script is missing"

D="$TMPDIR_FIXTURES/t27"; mkdir -p "$D/.devcontainer"
cat > "$D/.devcontainer/devcontainer.json" <<'EOF'
{
  // JSONC comments and trailing commas are valid in devcontainer.json.
  "name": "Test",
  "mounts": [
    "source=signal-fish-cargo-registry,target=/home/vscode/.cargo/registry,type=volume",
  ],
}
EOF
_run_test "$D" 0 "Accepts: JSONC comments with trailing commas"

# ── Group 4: Required host-home credential mounts ───────────────────────────

echo ""
echo -e "${YELLOW}Group 4: Required host-home credential bind mounts${NC}"

D="$TMPDIR_FIXTURES/t30"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=${localEnv:HOME}/.ssh,target=/home/vscode/.ssh,type=bind,readonly"]'
_run_test "$D" 1 "Rejects: HOME-based ~/.ssh bind mount"

D="$TMPDIR_FIXTURES/t31"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=${localEnv:USERPROFILE}/.gitconfig,target=/home/vscode/.gitconfig,type=bind,readonly"]'
_run_test "$D" 1 "Rejects: USERPROFILE-based ~/.gitconfig bind mount"

D="$TMPDIR_FIXTURES/t32"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=${localEnv:HOME}${localEnv:USERPROFILE}/.gnupg,target=/home/vscode/.gnupg,type=bind,readonly"]'
_run_test "$D" 1 "Rejects: combined home-variable ~/.gnupg bind mount"

D="$TMPDIR_FIXTURES/t33"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '[{"source":"${localEnv:HOME}/.ssh","target":"/home/vscode/.ssh","type":"bind"}]'
_run_test "$D" 1 "Rejects: object-form host credential bind mount"

D="$TMPDIR_FIXTURES/t34"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=C:\\Users\\alice\\.ssh,target=/home/vscode/.ssh,type=bind,readonly"]'
_run_test "$D" 1 "Rejects: hard-coded Windows SSH credential bind mount"

D="$TMPDIR_FIXTURES/t35"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=signal-fish-cargo-registry,target=/home/vscode/.cargo/registry,type=volume"]'
_run_test "$D" 0 "Accepts: named cache volume"

D="$TMPDIR_FIXTURES/t36"; mkdir -p "$D"
_make_fixture "$D" "MISSING" '["source=${localWorkspaceFolder}/fixtures,target=/fixtures,type=bind"]'
_run_test "$D" 0 "Accepts: workspace-relative non-credential bind mount"

echo ""

# ── Summary ───────────────────────────────────────────────────────────────────

echo -e "${BOLD}${YELLOW}=== Summary ===${NC}"
echo -e "  ${GREEN}Passed${NC}: $CHECKS_PASSED"
echo -e "  ${RED}Failed${NC}: $CHECKS_FAILED"
echo -e "  Total:  $CHECKS_RUN"
echo ""

if [ "$CHECKS_FAILED" -gt 0 ]; then
    echo -e "${RED}FAIL: $CHECKS_FAILED test(s) failed.${NC}"
    exit 1
else
    echo -e "${GREEN}PASS: All $CHECKS_PASSED tests passed.${NC}"
    exit 0
fi
