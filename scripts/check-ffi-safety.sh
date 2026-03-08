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
#   3. Emscripten FFI modules must have a compile_error!() target guard to
#      prevent compilation on non-Emscripten targets.
#   4. In files with a callback SAFETY block comment, every `extern "C" fn`
#      must have a per-function `// SAFETY:` comment on the line immediately
#      preceding its declaration.
#   5. Any `fn close()` method that calls `emscripten_websocket_close` must also
#      call `emscripten_websocket_delete` in that same method (not just in Drop)
#      to prevent late callback delivery between close() returning and Drop.
#   6. (Retired) Previously enforced explicit `&` in `.will_wake()` calls.
#      Retired because nightly clippy flags the explicit `&` as `needless_borrow`
#      and the emscripten CI job now runs clippy on the actual target.
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
            if [ "$in_repr_c" = true ] && echo "$line" | grep -qE '^[[:space:]]*(pub[[:space:]]+)?struct[[:space:]]+'; then
                in_struct=true
                struct_name=$(echo "$line" | grep -oE 'struct[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' | sed 's/struct[[:space:]]*//')
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
                if echo "$line" | grep -qE '^[[:space:]]*$|^[[:space:]]*#\[|^[[:space:]]*///'; then
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
                if echo "$line" | grep -v '^[[:space:]]*//' | grep -qE ':[[:space:]]*bool[[:space:]]*[,}]?[[:space:]]*$'; then
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
            # Strip leading whitespace (pure bash, avoids SC2001)
            trimmed="${code#"${code%%[![:space:]]*}"}"

            # Skip lines where the call is clearly inside an expression:
            #   - Line contains `let ... =` before the call
            #   - Line contains `=` before the call (assignment)
            #   - Line contains `if ` before the call
            if echo "$code" | grep -qE '(let[[:space:]]+.*=|=[[:space:]]*|if[[:space:]]+).*emscripten_websocket_set_'; then
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
                    # Strip leading whitespace (pure bash, avoids SC2001)
                    prev_trimmed="${prev_line#"${prev_line%%[![:space:]]*}"}"
                    # Skip blank lines and comment-only lines
                    if [ -z "$prev_trimmed" ] || echo "$prev_trimmed" | grep -qE '^[[:space:]]*//'; then
                        idx=$((idx - 1))
                        continue
                    fi
                    # If the previous meaningful line ends with ( or , or =
                    # or contains "let", it means this call is inside an expression.
                    if echo "$prev_trimmed" | grep -qE '[,(=][[:space:]]*$'; then
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

# ── Check 3: Target guard for Emscripten FFI modules ─────────────────
# Files that declare or call Emscripten-specific FFI functions must contain
# a compile_error!() guard to prevent compilation on non-Emscripten targets.
echo -e "${YELLOW}Check 3: Scanning for missing target guards in Emscripten FFI modules...${NC}"

CHECK3_VIOLATIONS=0

EMSCRIPTEN_FFI_FILES=$(grep -rl 'emscripten_websocket_new\|emscripten_websocket_set_' src/ 2>/dev/null || true)

if [ -z "$EMSCRIPTEN_FFI_FILES" ]; then
    echo -e "${GREEN}  No Emscripten FFI files found — nothing to check.${NC}"
else
    for file in $EMSCRIPTEN_FFI_FILES; do
        if ! grep -q 'compile_error!' "$file"; then
            echo -e "${RED}VIOLATION:${NC} $file: Emscripten FFI module missing compile_error!() target guard"
            echo "  Files using Emscripten C API must include:"
            echo "    #[cfg(not(target_os = \"emscripten\"))]"
            echo "    compile_error!(\"...\");"
            CHECK3_VIOLATIONS=$((CHECK3_VIOLATIONS + 1))
        elif ! grep -q 'cfg(not(target_os = "emscripten"))' "$file"; then
            echo -e "${RED}VIOLATION:${NC} $file: compile_error!() found but missing #[cfg(not(target_os = \"emscripten\"))] guard"
            echo "  The compile_error!() must be gated on non-Emscripten targets."
            CHECK3_VIOLATIONS=$((CHECK3_VIOLATIONS + 1))
        fi
    done
fi

VIOLATIONS=$((VIOLATIONS + CHECK3_VIOLATIONS))

if [ "$CHECK3_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}  Check 3: PASS — all Emscripten FFI modules have target guards.${NC}"
fi
echo ""

# ── Check 4: Callback SAFETY comment consistency ─────────────────────
# In files that have a SAFETY block comment covering callbacks (containing
# both "SAFETY" and "callback" within a comment block), every `extern "C" fn`
# must have a `// SAFETY:` comment on the line immediately preceding it.
echo -e "${YELLOW}Check 4: Scanning for missing per-function SAFETY comments on extern \"C\" fn callbacks...${NC}"

CHECK4_VIOLATIONS=0

EXTERN_C_FILES=$(grep -rl 'extern "C" fn' src/ 2>/dev/null || true)

if [ -z "$EXTERN_C_FILES" ]; then
    echo -e "${GREEN}  No extern \"C\" fn declarations found — nothing to check.${NC}"
else
    for file in $EXTERN_C_FILES; do
        # Check if this file has a callback SAFETY block comment.
        # Look for a comment line containing both "SAFETY" and "callback" (case-sensitive).
        has_safety_block=false
        if grep -q '// SAFETY.*callback\|// SAFETY.*Callback' "$file"; then
            has_safety_block=true
        fi

        if [ "$has_safety_block" = false ]; then
            continue
        fi

        # File has a callback SAFETY block — check each extern "C" fn.
        mapfile -t file_lines < "$file"
        total_lines=${#file_lines[@]}

        for ((i = 0; i < total_lines; i++)); do
            line="${file_lines[$i]}"
            line="${line//$'\r'/}"

            # Skip lines inside extern "C" { } blocks (FFI declarations, not callback definitions).
            # We only care about standalone extern "C" fn definitions.
            if echo "$line" | grep -qE '^[[:space:]]*extern "C" fn '; then
                lineno=$((i + 1))
                # Walk backwards to find the nearest non-blank line.
                prev_idx=$((i - 1))
                prev_line=""
                while [ "$prev_idx" -ge 0 ]; do
                    candidate="${file_lines[$prev_idx]}"
                    candidate="${candidate//$'\r'/}"
                    trimmed="${candidate#"${candidate%%[![:space:]]*}"}"
                    if [ -n "$trimmed" ]; then
                        prev_line="$trimmed"
                        break
                    fi
                    prev_idx=$((prev_idx - 1))
                done

                # Check if the previous non-blank line is a // SAFETY: comment.
                if ! echo "$prev_line" | grep -qE '^// SAFETY:'; then
                    fn_name=$(echo "$line" | grep -oE 'fn [A-Za-z_][A-Za-z0-9_]*' | sed 's/fn //')
                    echo -e "${RED}VIOLATION:${NC} $file:$lineno: extern \"C\" fn '$fn_name' is missing a // SAFETY: comment on the preceding line"
                    echo "  $line"
                    echo "  Add: // SAFETY: See the callback SAFETY block comment above for pointer guarantees."
                    CHECK4_VIOLATIONS=$((CHECK4_VIOLATIONS + 1))
                fi
            fi
        done
    done
fi

VIOLATIONS=$((VIOLATIONS + CHECK4_VIOLATIONS))

if [ "$CHECK4_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}  Check 4: PASS — all extern \"C\" fn callbacks have SAFETY comments.${NC}"
fi
echo ""

# ── Check 5: close() must also call delete to unregister callbacks ────
# If a file calls emscripten_websocket_close inside a Transport trait impl
# close() method, it must also call emscripten_websocket_delete in that same
# method. Without this, callbacks remain registered between close() and Drop,
# creating a window for late callback delivery.
echo -e "${YELLOW}Check 5: Scanning for close() methods that close but do not delete...${NC}"

CHECK5_VIOLATIONS=0

CLOSE_FILES=$(grep -rl 'emscripten_websocket_close' src/ 2>/dev/null || true)

if [ -z "$CLOSE_FILES" ]; then
    echo -e "${GREEN}  No files with emscripten_websocket_close found — nothing to check.${NC}"
else
    for file in $CLOSE_FILES; do
        # Find all fn close() method bodies and check if they contain both
        # emscripten_websocket_close AND emscripten_websocket_delete.
        # Use awk to extract close() method bodies.
        in_close_fn=false
        brace_depth=0
        has_ws_close=false
        has_ws_delete=false
        close_start_line=0

        lineno=0
        while IFS= read -r line; do
            line="${line//$'\r'/}"
            lineno=$((lineno + 1))

            # Detect `async fn close` or `fn close` method signature
            if echo "$line" | grep -qE '(async[[:space:]]+)?fn[[:space:]]+close[[:space:]]*\('; then
                in_close_fn=true
                brace_depth=0
                has_ws_close=false
                has_ws_delete=false
                close_start_line=$lineno
            fi

            if [ "$in_close_fn" = true ]; then
                opens=$(echo "$line" | tr -cd '{' | wc -c)
                closes=$(echo "$line" | tr -cd '}' | wc -c)
                brace_depth=$((brace_depth + opens - closes))

                if echo "$line" | grep -qv '^\s*//' && echo "$line" | grep -q 'emscripten_websocket_close'; then
                    has_ws_close=true
                fi
                if echo "$line" | grep -qv '^\s*//' && echo "$line" | grep -q 'emscripten_websocket_delete'; then
                    has_ws_delete=true
                fi

                if [ "$brace_depth" -le 0 ] && [ "$close_start_line" -ne "$lineno" ]; then
                    if [ "$has_ws_close" = true ] && [ "$has_ws_delete" = false ]; then
                        echo -e "${RED}VIOLATION:${NC} $file:$close_start_line: close() calls emscripten_websocket_close but NOT emscripten_websocket_delete"
                        echo "  close() must also call emscripten_websocket_delete to unregister callbacks."
                        echo "  Without this, callbacks can fire between close() returning and Drop running."
                        CHECK5_VIOLATIONS=$((CHECK5_VIOLATIONS + 1))
                    fi
                    in_close_fn=false
                fi
            fi
        done < "$file"
    done
fi

VIOLATIONS=$((VIOLATIONS + CHECK5_VIOLATIONS))

if [ "$CHECK5_VIOLATIONS" -eq 0 ]; then
    echo -e "${GREEN}  Check 5: PASS — all close() methods that close also delete/unregister.${NC}"
fi
echo ""

# ── Check 6: (retired) ────────────────────────────────────────────────
# Previously scanned for .will_wake() calls missing an explicit &
# reference argument. This check is retired because:
#   1. Nightly clippy (used by the emscripten CI job) flags the explicit &
#      as `needless_borrow` — the compiler auto-refs owned Waker values.
#   2. The emscripten CI job now runs `cargo clippy` on the actual target,
#      so type errors in cfg-guarded code are caught by the compiler.
#   3. Both `.will_wake(noop)` and `.will_wake(&noop)` are valid Rust;
#      the former is preferred by clippy to avoid needless borrows.
echo -e "${GREEN}  Check 6: SKIP — retired (.will_wake ref check now handled by clippy).${NC}"
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
