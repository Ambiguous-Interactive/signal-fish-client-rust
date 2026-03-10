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
    #
    # Match lines where grep is invoked with -P as part of the flags.
    # We look for patterns like:
    #   grep -P
    #   grep -oP
    #   grep -Pq
    #   grep -Pn
    #   grep -cP
    #   grep -qP
    #   grep -nP
    #   grep -[any combo containing P]
    #
    # We exclude:
    #   - Lines starting with # (comments)
    #   - Lines where -P is inside a quoted string argument to echo/printf
    #
    # The regex: a line that is NOT a comment, contains "grep" followed by
    # whitespace and a dash-flag-group containing P.
    local grep_p_violations=0
    local line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Skip blank lines
        [ -z "$line" ] && continue

        # Strip leading whitespace for comment detection
        local stripped
        stripped="${line#"${line%%[![:space:]]*}"}"

        # Skip comment lines
        [[ "$stripped" == \#* ]] && continue

        # Skip lines that are just echo/printf strings describing grep -P
        # (e.g., usage messages or documentation)
        if printf '%s\n' "$stripped" | grep -qE '^(echo|printf)[[:space:]]'; then
            continue
        fi

        # Check for grep invoked with -P in its flags.
        # The pattern matches: grep followed by whitespace then a
        # short-option group containing P anywhere (e.g., -P, -oP, -Pq,
        # -Pn, -cP, -qP, -nP, etc.).
        # We use -E (ERE) to stay portable.
        # The regex matches P anywhere in the option group (not just at
        # the end) using two alternations: P followed by more flags, or
        # P at the end. Combined into one pattern with alternation.
        if printf '%s\n' "$line" | grep -qE 'grep[[:space:]]+-[a-zA-Z]*P[a-zA-Z]*([[:space:]]|$)'; then
            echo "  VIOLATION: $file:$line_num: grep -P (PCRE)"
            echo "    $line"
            grep_p_violations=$((grep_p_violations + 1))
        fi
    done < "$file"

    if [ "$grep_p_violations" -gt 0 ]; then
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + grep_p_violations))
    fi

    # ── Check 2: sed -r (GNU-only, should use sed -E) ────────────────
    local sed_r_violations=0
    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Skip blank lines
        [ -z "$line" ] && continue

        # Strip leading whitespace for comment detection
        local stripped
        stripped="${line#"${line%%[![:space:]]*}"}"

        # Skip comment lines
        [[ "$stripped" == \#* ]] && continue

        # Skip echo/printf strings
        if printf '%s\n' "$stripped" | grep -qE '^(echo|printf)[[:space:]]'; then
            continue
        fi

        # Check for sed invoked with -r flag (r can appear anywhere in
        # the short-option group, e.g., -r, -ri, -rn, -ir, -nr, etc.)
        if printf '%s\n' "$line" | grep -qE 'sed[[:space:]]+-[a-zA-Z]*r[a-zA-Z]*([[:space:]]|$)'; then
            echo "  VIOLATION: $file:$line_num: sed -r (GNU-only, use sed -E)"
            echo "    $line"
            sed_r_violations=$((sed_r_violations + 1))
        fi
    done < "$file"

    if [ "$sed_r_violations" -gt 0 ]; then
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + sed_r_violations))
    fi

    # ── Check 3: PCRE shorthand (\s, \w, \d, \S, \W, \D) in grep -E ──
    #
    # These character-class shorthands are PCRE extensions. They work in
    # GNU grep -E (as a non-standard extension) but are NOT part of
    # POSIX ERE and will silently misbehave or error on macOS/BSD grep.
    #
    # Portable replacements:
    #   \s -> [[:space:]]     \S -> [^[:space:]]
    #   \w -> [[:alnum:]_]    \W -> [^[:alnum:]_]
    #   \d -> [[:digit:]]     \D -> [^[:digit:]]
    #
    local pcre_shorthand_violations=0
    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Skip blank lines
        [ -z "$line" ] && continue

        # Strip leading whitespace for comment detection
        local stripped
        stripped="${line#"${line%%[![:space:]]*}"}"

        # Skip comment lines
        [[ "$stripped" == \#* ]] && continue

        # Skip echo/printf strings
        if printf '%s\n' "$stripped" | grep -qE '^(echo|printf)[[:space:]]'; then
            continue
        fi

        # Only check lines that invoke grep with -E (or combined flags
        # like -qE, -cE, -oE, -nE, etc.)
        if ! printf '%s\n' "$line" | grep -qE 'grep[[:space:]]+-[a-zA-Z]*E'; then
            continue
        fi

        # Now check if the line contains PCRE shorthand sequences
        # We look for backslash followed by s, w, d, S, W, or D
        if printf '%s\n' "$line" | grep -qE '\\[swdSWD]'; then
            echo "  VIOLATION: $file:$line_num: PCRE shorthand in grep -E (not POSIX ERE)"
            echo "    $line"
            pcre_shorthand_violations=$((pcre_shorthand_violations + 1))
        fi
    done < "$file"

    if [ "$pcre_shorthand_violations" -gt 0 ]; then
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + pcre_shorthand_violations))
    fi

    # ── Check 4: PCRE shorthand (\s, \w, \d, etc.) in sed expressions ──
    #
    # These shorthands are NOT part of POSIX BRE or ERE. GNU sed treats
    # \s as [[:space:]], but macOS/BSD sed treats \s as literal 's'.
    # This causes silent incorrect behavior.
    #
    # Portable replacements:
    #   \s -> [[:space:]]     \S -> [^[:space:]]
    #   \w -> [[:alnum:]_]    \W -> [^[:alnum:]_]
    #   \d -> [[:digit:]]     \D -> [^[:digit:]]
    #
    local sed_pcre_violations=0
    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Skip blank lines
        [ -z "$line" ] && continue

        # Strip leading whitespace for comment detection
        local stripped
        stripped="${line#"${line%%[![:space:]]*}"}"

        # Skip comment lines
        [[ "$stripped" == \#* ]] && continue

        # Skip echo/printf strings
        if printf '%s\n' "$stripped" | grep -qE '^(echo|printf)[[:space:]]'; then
            continue
        fi

        # Only check lines that invoke sed
        if ! printf '%s\n' "$line" | grep -qE 'sed[[:space:]]'; then
            continue
        fi

        # Check if the line contains PCRE shorthand sequences in sed expressions
        if printf '%s\n' "$line" | grep -qE '\\[swdSWD]'; then
            echo "  VIOLATION: $file:$line_num: PCRE shorthand in sed expression (not POSIX)"
            echo "    $line"
            sed_pcre_violations=$((sed_pcre_violations + 1))
        fi
    done < "$file"

    if [ "$sed_pcre_violations" -gt 0 ]; then
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + sed_pcre_violations))
    fi

    # ── Check 5: \b word boundary in grep or sed (GNU extension) ──────
    #
    # \b is a GNU extension for word boundaries. It is NOT part of
    # POSIX BRE or ERE. macOS/BSD grep and sed do not support it.
    #
    # Portable replacements:
    #   grep -w              for whole-word matching
    #   (^|[^[:alnum:]_])    for leading word boundary
    #   ([^[:alnum:]_]|$)    for trailing word boundary
    #
    local word_boundary_violations=0
    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))

        # Skip blank lines
        [ -z "$line" ] && continue

        # Strip leading whitespace for comment detection
        local stripped
        stripped="${line#"${line%%[![:space:]]*}"}"

        # Skip comment lines
        [[ "$stripped" == \#* ]] && continue

        # Skip echo/printf strings
        if printf '%s\n' "$stripped" | grep -qE '^(echo|printf)[[:space:]]'; then
            continue
        fi

        # Only check lines that invoke grep or sed
        if ! printf '%s\n' "$line" | grep -qE '(grep|sed)[[:space:]]'; then
            continue
        fi

        # Check for \b word boundary
        if printf '%s\n' "$line" | grep -qE '\\b'; then
            printf '%s\n' "  VIOLATION: $file:$line_num: \\b word boundary (GNU extension, not POSIX)"
            echo "    $line"
            word_boundary_violations=$((word_boundary_violations + 1))
        fi
    done < "$file"

    if [ "$word_boundary_violations" -gt 0 ]; then
        FILE_VIOLATIONS=$((FILE_VIOLATIONS + word_boundary_violations))
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
