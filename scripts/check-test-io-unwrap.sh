#!/usr/bin/env bash
# check-test-io-unwrap.sh — Verify Rust test files use descriptive error
# handling on I/O operations instead of bare .unwrap().
#
# Catches patterns like:
#   read_dir(...).unwrap()
#   read_to_string(...).unwrap()
#   File::open(...).unwrap()
#   File::create(...).unwrap()
#   write_all(...).unwrap()
#   OpenOptions::new()...unwrap()
#
# The project convention is to use .unwrap_or_else(|e| panic!(...)) with a
# descriptive message that includes the file path and error, instead of bare
# .unwrap() on I/O operations. This produces better error messages when
# tests fail due to missing files or permission errors.
#
# Scope: files in tests/ only. While src/ files are collected, they are
# skipped entirely because detecting #[cfg(test)] module boundaries
# reliably in a shell script is complex (multi-line parsing, nested
# modules, conditional compilation). The tests/ directory is the
# primary target for this check.
#
# Exit codes:
#   0 — no violations found
#   1 — one or more violations detected
#
# Usage:
#   bash scripts/check-test-io-unwrap.sh

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

echo -e "${YELLOW}=== Rust test I/O unwrap check ===${NC}"
echo ""
echo "Scanning for bare .unwrap() on I/O operations in Rust test files..."
echo ""

# I/O function patterns to check.
# Each pattern matches an I/O call immediately followed by .unwrap()
# on the same line.
#
# We use POSIX ERE (grep -E) for portability (no -P).
IO_PATTERNS=(
    'read_dir[[:space:]]*\(.*\)\.unwrap\(\)'
    'read_to_string[[:space:]]*\(.*\)\.unwrap\(\)'
    'File::open[[:space:]]*\(.*\)\.unwrap\(\)'
    'File::create[[:space:]]*\(.*\)\.unwrap\(\)'
    'write_all[[:space:]]*\(.*\)\.unwrap\(\)'
    'OpenOptions::new\(\).*\.unwrap\(\)'
)

# Also check multiline patterns where .unwrap() appears on a line by
# itself or after a closing paren, following an I/O call on the
# previous line. We handle this with a two-pass approach below.
IO_CALL_STARTERS=(
    'read_dir[[:space:]]*\('
    'read_to_string[[:space:]]*\('
    'File::open[[:space:]]*\('
    'File::create[[:space:]]*\('
    'write_all[[:space:]]*\('
    'OpenOptions::new\(\)'
)

# Collect Rust files to scan (tests/ directory only; src/ files are
# skipped because detecting #[cfg(test)] boundaries in shell is complex).
RS_FILES=()

if [ -d "$REPO_ROOT/tests" ]; then
    while IFS= read -r -d '' f; do
        RS_FILES+=("$f")
    done < <(find "$REPO_ROOT/tests" -name '*.rs' -print0 2>/dev/null)
fi

if [ -d "$REPO_ROOT/src" ]; then
    while IFS= read -r -d '' f; do
        RS_FILES+=("$f")
    done < <(find "$REPO_ROOT/src" -name '*.rs' -print0 2>/dev/null)
fi

if [ "${#RS_FILES[@]}" -eq 0 ]; then
    echo -e "${GREEN}No Rust files found to check.${NC}"
    exit 0
fi

echo "Found ${#RS_FILES[@]} Rust file(s) to scan."
echo ""

for file in "${RS_FILES[@]}"; do
    rel_path="${file#"$REPO_ROOT"/}"
    is_test_file=false

    # Files under tests/ are always test files.
    case "$rel_path" in
        tests/*) is_test_file=true ;;
    esac

    # For src/ files, only check lines inside #[cfg(test)] modules.
    # For simplicity (and to avoid complex multi-line parsing), we skip
    # src/ files entirely in this check. The tests/ directory is the
    # primary target, and src/ test modules are typically small.
    if [ "$is_test_file" = false ]; then
        continue
    fi

    file_violations=0

    # ── Single-line check ────────────────────────────────────────────
    # Use whole-file grep -nE per pattern instead of per-line iteration.
    # This reduces ~52,000 subprocess spawns to ~24 (6 patterns × 4 files).
    for pattern in "${IO_PATTERNS[@]}"; do
        raw=$(grep -nE "$pattern" "$file" 2>/dev/null || true)
        [ -z "$raw" ] && continue
        while IFS= read -r match; do
            lineno="${match%%:*}"
            content="${match#*:}"
            stripped="${content#"${content%%[![:space:]]*}"}"
            [[ "$stripped" == //* ]] && continue
            echo -e "${RED}VIOLATION:${NC} $rel_path:$lineno: bare .unwrap() on I/O operation"
            echo "  $stripped"
            file_violations=$((file_violations + 1))
        done <<< "$raw"
    done

    # ── Multiline check ──────────────────────────────────────────────
    # Check for cases where an I/O call spans lines and .unwrap() is on
    # a subsequent line (e.g., read_dir(...)\n    .unwrap()).
    #
    # Instead of iterating every line, first grep for just the .unwrap()
    # continuation lines, then only look back for those few matches.
    # This reduces ~1.5M iterations to just the few matching lines.
    unwrap_raw=$(grep -nE '^[[:space:]]*\.unwrap\(\)' "$file" 2>/dev/null || true)
    if [ -n "$unwrap_raw" ]; then
        mapfile -t file_lines < "$file"
        while IFS= read -r match; do
            lineno="${match%%:*}"
            i=$((lineno - 1))  # convert to 0-indexed

            # Look back up to 5 lines for nearest non-blank, non-comment line
            for ((j = i - 1; j >= 0 && j >= i - 5; j--)); do
                prev="${file_lines[$j]}"
                prev="${prev//$'\r'/}"
                prev_stripped="${prev#"${prev%%[![:space:]]*}"}"
                [ -z "$prev_stripped" ] && continue
                [[ "$prev_stripped" == //* ]] && continue

                # Check if this previous line has an I/O call starter
                for starter in "${IO_CALL_STARTERS[@]}"; do
                    if printf '%s\n' "$prev" | grep -qE "$starter"; then
                        # Make sure this isn't already caught by the single-line check
                        # (i.e., the same line doesn't also have .unwrap())
                        if ! printf '%s\n' "$prev" | grep -qE '\.unwrap\(\)'; then
                            echo -e "${RED}VIOLATION:${NC} $rel_path:$lineno: bare .unwrap() on I/O operation (multiline)"
                            echo "  $prev_stripped"
                            printf '  %s\n' "$(printf '%s\n' "${file_lines[$i]}" | sed 's/^[[:space:]]*//')"
                            file_violations=$((file_violations + 1))
                        fi
                    fi
                done
                break  # Only check the nearest non-blank line
            done
        done <<< "$unwrap_raw"
    fi

    if [ "$file_violations" -gt 0 ]; then
        VIOLATIONS=$((VIOLATIONS + file_violations))
    fi
done

echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS violation(s) found.${NC}"
    echo ""
    echo "Use .unwrap_or_else(|e| panic!(...)) with a descriptive message instead:"
    echo ""
    echo "  // Instead of:"
    echo "  std::fs::read_to_string(&path).unwrap()"
    echo ""
    echo "  // Use:"
    echo '  std::fs::read_to_string(&path).unwrap_or_else(|e| {'
    echo '      panic!("Failed to read '"'"'{}'"'"': {e}", path.display());'
    echo '  })'
    exit 1
else
    echo -e "${GREEN}PASSED: No bare .unwrap() on I/O operations in test files.${NC}"
    exit 0
fi
