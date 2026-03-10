#!/usr/bin/env bash
# test_shell_portability.sh — Verify shell scripts avoid non-portable constructs.
#
# Catches:
#   - grep -P / grep -oP (GNU PCRE — breaks on macOS/BSD)
#   - sed -r (GNU-only — use sed -E for portability)
#   - \s, \w, \d, \S, \W, \D in grep -E patterns (PCRE shorthand, not POSIX ERE)
#   - \s, \w, \d, \S, \W, \D in sed expressions (not POSIX — macOS/BSD treats as literal)
#   - \b word boundaries in grep/sed (GNU extension, not POSIX ERE/BRE)
#
# Usage:
#   bash scripts/test_shell_portability.sh

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Counters ──────────────────────────────────────────────────────────
CHECKS_RUN=0
CHECKS_PASSED=0
CHECKS_FAILED=0
VIOLATIONS_TOTAL=0

# ── Helpers ───────────────────────────────────────────────────────────

# _run_check: Grep for a pattern in a file, filter non-code lines, report violations.
#   $1 — file path
#   $2 — grep -nE pattern
#   $3 — violation description
# Increments FILE_VIOLATIONS for each violation found.
_run_check() {
    local file="$1" pattern="$2" desc="$3"
    local raw
    raw=$(grep -nE "$pattern" "$file" 2>/dev/null || true)
    [ -z "$raw" ] && return
    while IFS= read -r match; do
        local lineno="${match%%:*}"
        local content="${match#*:}"
        local stripped="${content#"${content%%[![:space:]]*}"}"
        [[ "$stripped" == \#* ]] && continue
        case "$stripped" in echo\ *|printf\ *) continue ;; esac
        echo "  VIOLATION: $file:$lineno: $desc"
        echo "    $content"
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + 1))
    done <<< "$raw"
}

# Check a single file for non-portable constructs.
# Populates FILE_VIOLATIONS with the count for the current file.
#   $1 — path to the shell script to check
check_file() {
    local file="$1"
    local basename
    basename="$(basename "$file")"
    FILE_VIOLATIONS=0

    # Skip ourselves — we reference grep -P patterns in comments/strings
    # as part of explaining what we check for.
    if [ "$basename" = "test_shell_portability.sh" ]; then
        return
    fi

    # ── Check 1: grep with -P flag (PCRE) ────────────────────────────
    _run_check "$file" \
        'grep[[:space:]]+-[a-zA-Z]*P[a-zA-Z]*([[:space:]]|$)' \
        'grep -P (PCRE)'

    # ── Check 2: sed -r (GNU-only, should use sed -E) ────────────────
    _run_check "$file" \
        'sed[[:space:]]+-[a-zA-Z]*r[a-zA-Z]*([[:space:]]|$)' \
        'sed -r (GNU-only, use sed -E)'

    # ── Check 3: PCRE shorthand (\s, \w, \d, \S, \W, \D) in grep -E ──
    #
    # Two-step: first find lines invoking grep -E, then check each for
    # PCRE shorthand sequences. The inner per-match grep is acceptable
    # because grep -E lines are rare in any given file.
    local grep_e_raw
    grep_e_raw=$(grep -nE 'grep[[:space:]]+-[a-zA-Z]*E' "$file" 2>/dev/null || true)
    if [ -n "$grep_e_raw" ]; then
        while IFS= read -r match; do
            local lineno="${match%%:*}"
            local content="${match#*:}"
            local stripped="${content#"${content%%[![:space:]]*}"}"
            [[ "$stripped" == \#* ]] && continue
            case "$stripped" in echo\ *|printf\ *) continue ;; esac
            if printf '%s\n' "$content" | grep -qE '\\[swdSWD]'; then
                echo "  VIOLATION: $file:$lineno: PCRE shorthand in grep -E (not POSIX ERE)"
                echo "    $content"
                FILE_VIOLATIONS=$((FILE_VIOLATIONS + 1))
            fi
        done <<< "$grep_e_raw"
    fi

    # ── Check 4: PCRE shorthand (\s, \w, \d, etc.) in sed expressions ──
    #
    # Two-step: first find lines invoking sed, then check each for
    # PCRE shorthand sequences.
    local sed_raw
    sed_raw=$(grep -nE 'sed[[:space:]]' "$file" 2>/dev/null || true)
    if [ -n "$sed_raw" ]; then
        while IFS= read -r match; do
            local lineno="${match%%:*}"
            local content="${match#*:}"
            local stripped="${content#"${content%%[![:space:]]*}"}"
            [[ "$stripped" == \#* ]] && continue
            case "$stripped" in echo\ *|printf\ *) continue ;; esac
            if printf '%s\n' "$content" | grep -qE '\\[swdSWD]'; then
                echo "  VIOLATION: $file:$lineno: PCRE shorthand in sed expression (not POSIX)"
                echo "    $content"
                FILE_VIOLATIONS=$((FILE_VIOLATIONS + 1))
            fi
        done <<< "$sed_raw"
    fi

    # ── Check 5: \b word boundary in grep or sed (GNU extension) ──────
    #
    # Two-step: first find lines invoking grep or sed, then check each
    # for \b word boundary usage.
    local gs_raw
    gs_raw=$(grep -nE '(grep|sed)[[:space:]]' "$file" 2>/dev/null || true)
    if [ -n "$gs_raw" ]; then
        while IFS= read -r match; do
            local lineno="${match%%:*}"
            local content="${match#*:}"
            local stripped="${content#"${content%%[![:space:]]*}"}"
            [[ "$stripped" == \#* ]] && continue
            case "$stripped" in echo\ *|printf\ *) continue ;; esac
            if printf '%s\n' "$content" | grep -qE '\\b'; then
                printf '  VIOLATION: %s:%s: \\b word boundary (GNU extension, not POSIX)\n' "$file" "$lineno"
                echo "    $content"
                FILE_VIOLATIONS=$((FILE_VIOLATIONS + 1))
            fi
        done <<< "$gs_raw"
    fi
}

# ── Main ──────────────────────────────────────────────────────────────

echo "=== Shell portability check ==="
echo "Scanning .sh files in: $REPO_ROOT/scripts/"
echo ""

# Collect all .sh files in scripts/
SCRIPT_FILES=()
for f in "$REPO_ROOT/scripts/"*.sh; do
    [ -f "$f" ] && SCRIPT_FILES+=("$f")
done

if [ "${#SCRIPT_FILES[@]}" -eq 0 ]; then
    echo "No .sh files found in scripts/ — nothing to check."
    exit 0
fi

echo "Found ${#SCRIPT_FILES[@]} script(s) to check."
echo ""

for script_file in "${SCRIPT_FILES[@]}"; do
    CHECKS_RUN=$((CHECKS_RUN + 1))
    FILE_VIOLATIONS=0

    check_file "$script_file"

    if [ "$FILE_VIOLATIONS" -eq 0 ]; then
        echo "  PASS: $(basename "$script_file")"
        CHECKS_PASSED=$((CHECKS_PASSED + 1))
    else
        echo "  FAIL: $(basename "$script_file") ($FILE_VIOLATIONS violation(s))"
        CHECKS_FAILED=$((CHECKS_FAILED + 1))
        VIOLATIONS_TOTAL=$((VIOLATIONS_TOTAL + FILE_VIOLATIONS))
    fi
done

echo ""
echo "=== Results ==="
echo "Scripts checked: $CHECKS_RUN"
echo "Scripts clean:   $CHECKS_PASSED"
echo "Scripts failing: $CHECKS_FAILED"
echo "Total violations: $VIOLATIONS_TOTAL"

if [ "$CHECKS_FAILED" -gt 0 ]; then
    echo ""
    echo "FAILED: $CHECKS_FAILED script(s) contain non-portable constructs."
    echo ""
    echo "Fixes:"
    echo "  grep -P  / grep -oP  -> grep -E / grep -oE / sed -nE / python"
    echo "  sed -r               -> sed -E"
    printf '%s\n' "  \\s in grep -E       -> [[:space:]]   (\\w->[[:alnum:]_], \\d->[[:digit:]])"
    printf '%s\n' "  \\s in sed           -> [[:space:]]   (\\w->[[:alnum:]_], \\d->[[:digit:]])"
    printf '%s\n' "  \\b in grep/sed      -> grep -w, or (^|[^[:alnum:]_])word([^[:alnum:]_]|$)"
    exit 1
else
    echo ""
    echo "ALL SCRIPTS PASSED portability checks."
    exit 0
fi
