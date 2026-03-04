#!/usr/bin/env bash
# check-ffi-safety.sh — Static analysis for common FFI ABI mistakes.
#
# Scans Rust source files that use `#[repr(C)]` structs and raw FFI calls
# for patterns that cause subtle ABI mismatches, especially on the
# wasm32-unknown-emscripten target.
#
# Checks:
#   1. #[repr(C)] structs must not contain bare `bool` fields — C ABI
#      expects `int` (4 bytes), but Rust `bool` is 1 byte. Use `c_int`,
#      `EM_BOOL`, or a similar integer type alias instead.
#   2. FFI callback-registration functions (`emscripten_websocket_set_*`)
#      must have their return values checked. Ignoring a failed registration
#      silently drops events.
#
# Exit codes:
#   0 — no violations found
#   1 — one or more violations detected

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

echo -e "${YELLOW}=== FFI safety check ===${NC}"
echo ""

# ── Check 1: bool in #[repr(C)] structs ─────────────────────────────
# Extract all #[repr(C)] struct blocks and flag any that contain a bare
# `bool` field. The regex looks for `: bool` or `: bool,` inside struct
# bodies that follow a #[repr(C)] annotation.
echo -e "${YELLOW}Check 1: Scanning for bare 'bool' fields in #[repr(C)] structs...${NC}"

# Find all .rs files that contain #[repr(C)]
REPR_C_FILES=$(grep -rl '#\[repr(C)\]' src/ 2>/dev/null || true)

if [ -z "$REPR_C_FILES" ]; then
    echo -e "${GREEN}  No #[repr(C)] structs found — nothing to check.${NC}"
else
    for file in $REPR_C_FILES; do
        # Use awk to extract struct bodies that follow #[repr(C)].
        # For each such block, check if any field uses bare `bool`.
        in_repr_c=false
        in_struct=false
        struct_name=""
        brace_depth=0
        lineno=0

        while IFS= read -r line; do
            line="${line//$'\r'/}"
            lineno=$((lineno + 1))

            # Detect #[repr(C)] annotation
            if echo "$line" | grep -q '#\[repr(C)\]'; then
                in_repr_c=true
                continue
            fi

            # Detect struct opening after #[repr(C)]
            if [ "$in_repr_c" = true ] && echo "$line" | grep -qE '^\s*(pub\s+)?struct\s+'; then
                in_struct=true
                struct_name=$(echo "$line" | grep -oE 'struct\s+[A-Za-z_][A-Za-z0-9_]*' | sed 's/struct\s*//')
                # Count opening braces on this line
                opens=$(echo "$line" | tr -cd '{' | wc -c)
                closes=$(echo "$line" | tr -cd '}' | wc -c)
                brace_depth=$((brace_depth + opens - closes))
                in_repr_c=false
                continue
            fi

            # If we hit something else after #[repr(C)], cancel it
            if [ "$in_repr_c" = true ]; then
                # Allow blank lines, attributes, and doc comments between #[repr(C)] and struct
                if echo "$line" | grep -qE '^\s*$|^\s*#\[|^\s*///'; then
                    continue
                fi
                in_repr_c=false
            fi

            # Inside a #[repr(C)] struct body
            if [ "$in_struct" = true ]; then
                opens=$(echo "$line" | tr -cd '{' | wc -c)
                closes=$(echo "$line" | tr -cd '}' | wc -c)
                brace_depth=$((brace_depth + opens - closes))

                # Check for bare bool field: `: bool` not preceded by `//`
                if echo "$line" | grep -v '^\s*//' | grep -qE ':\s*bool\s*[,}]?\s*$'; then
                    echo -e "${RED}VIOLATION:${NC} $file:$lineno: bare 'bool' in #[repr(C)] struct '$struct_name'"
                    echo "  $line"
                    echo "  Use c_int, EM_BOOL, or another integer type instead of bool in C FFI structs."
                    echo "  Rust bool is 1 byte, but C typically uses int (4 bytes) for boolean values."
                    VIOLATIONS=$((VIOLATIONS + 1))
                fi

                if [ "$brace_depth" -le 0 ]; then
                    in_struct=false
                    struct_name=""
                    brace_depth=0
                fi
            fi
        done < "$file"
    done
fi

if [ "$VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}  Check 1: PASS — no bare bool in #[repr(C)] structs.${NC}"
fi
echo ""

# ── Check 2: Unchecked FFI return values ─────────────────────────────
# FFI functions like emscripten_websocket_set_*_callback_on_thread return
# a result code that MUST be checked. Calling them as bare statements
# (without assigning or comparing the result) silently ignores failures.
echo -e "${YELLOW}Check 2: Scanning for unchecked FFI callback-registration return values...${NC}"

CHECK2_VIOLATIONS=0

FFI_FILES=$(grep -rl 'emscripten_websocket_set_' src/ 2>/dev/null || true)

if [ -z "$FFI_FILES" ]; then
    echo -e "${GREEN}  No FFI callback registrations found — nothing to check.${NC}"
else
    for file in $FFI_FILES; do
        # Look for bare calls that don't assign or compare the result.
        # A properly checked call looks like:
        #   let result = emscripten_websocket_set_...
        #   ("name", emscripten_websocket_set_...   (tuple pattern for batch checking)
        #   if emscripten_websocket_set_...          (direct comparison)
        # An unchecked call looks like a bare statement:
        #   emscripten_websocket_set_...(
        # We read the file line-by-line, tracking context, to distinguish
        # bare calls from calls inside expressions (let, tuples, if, etc.).
        matches=$(grep -n 'emscripten_websocket_set_' "$file" \
            | grep -v '^\s*//' \
            | grep -v '//.*emscripten_websocket_set_' \
            | grep -v 'fn emscripten_websocket_set_' \
            | grep -v 'type.*emscripten_websocket_set_' \
            || true)

        # Read the file into an array so we can inspect context lines.
        mapfile -t file_lines < "$file"

        while IFS= read -r match_line; do
            match_line="${match_line//$'\r'/}"
            [ -z "$match_line" ] && continue

            lineno=$(echo "$match_line" | cut -d: -f1)
            code=$(echo "$match_line" | cut -d: -f2-)
            trimmed=$(echo "$code" | sed 's/^[[:space:]]*//')

            # Skip lines where the call is clearly inside an expression:
            #   - Line contains `let ... =` before the call
            #   - Line contains `=` before the call (assignment)
            #   - Line contains `if ` before the call
            if echo "$code" | grep -qE '(let\s+.*=|=\s*|if\s+).*emscripten_websocket_set_'; then
                continue
            fi

            # If the line starts with the FFI call, check the preceding
            # non-blank line. If it's part of a tuple/array expression
            # (e.g., ("name",) or [(...),]), the return value is captured.
            if echo "$trimmed" | grep -qE '^emscripten_websocket_set_'; then
                checked=false
                # Walk backwards to find the nearest non-blank, non-comment line.
                idx=$((lineno - 2)) # 0-indexed, minus one more for previous line
                while [ "$idx" -ge 0 ]; do
                    prev_line="${file_lines[$idx]}"
                    prev_trimmed=$(echo "$prev_line" | sed 's/^[[:space:]]*//')
                    # Skip blank lines and comment-only lines
                    if [ -z "$prev_trimmed" ] || echo "$prev_trimmed" | grep -qE '^\s*//'; then
                        idx=$((idx - 1))
                        continue
                    fi
                    # If the previous meaningful line ends with ( or , or =
                    # or contains "let", it means this call is inside an expression.
                    if echo "$prev_trimmed" | grep -qE '[,(=]\s*$'; then
                        checked=true
                    fi
                    break
                done

                if [ "$checked" = false ]; then
                    echo -e "${RED}VIOLATION:${NC} $file:$lineno: unchecked FFI return value"
                    echo "  $code"
                    echo "  The return value of emscripten_websocket_set_* must be checked."
                    echo "  Assign it to a variable and verify it equals EMSCRIPTEN_RESULT_SUCCESS."
                    CHECK2_VIOLATIONS=$((CHECK2_VIOLATIONS + 1))
                fi
            fi
        done <<< "$matches"
    done
fi

VIOLATIONS=$((VIOLATIONS + CHECK2_VIOLATIONS))

if [ "$CHECK2_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}  Check 2: PASS — all FFI return values are checked.${NC}"
fi
echo ""

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS FFI safety violation(s) found.${NC}"
    echo "Fix all violations before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No FFI safety issues found.${NC}"
    exit 0
fi
