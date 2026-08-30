#!/usr/bin/env bash
# check-ffi-safety.sh — Static analysis for common FFI ABI mistakes.
#
# Scans Rust source files that use `#[repr(C)]` structs and raw FFI calls
# for patterns that cause subtle ABI mismatches, especially on the
# wasm32-unknown-emscripten target.
#
# Checks:
#   1. #[repr(C)] structs must not contain bare Rust `bool` fields. Bind the
#      header's exact ABI type explicitly: for example `C_BOOL = u8` for the
#      one-byte bool fields in Emscripten websocket structs, or `c_int` for
#      integer-sized `EM_BOOL` values.
#   2. FFI callback-registration functions (`emscripten_websocket_set_*`)
#      must have their return values checked. Ignoring a failed registration
#      silently drops events.
#   3. Emscripten FFI modules must have a compile_error!() target guard to
#      prevent compilation on non-Emscripten targets.
#   4. In files with a callback SAFETY block comment, every `extern "C" fn`
#      must have a per-function `// SAFETY:` comment on the line immediately
#      preceding its declaration.
#   5. Any transport `close`/`poll_close` method that closes an Emscripten socket
#      must also delete it in that same method, directly or through the audited
#      cleanup helpers, to prevent late callback delivery after close returns.
#   6. (Retired) Previously enforced explicit `&` in `.will_wake()` calls.
#      Retired because nightly clippy flags the explicit `&` as `needless_borrow`
#      and the emscripten CI job now runs clippy on the actual target.
#   7. Callback state has one owning wrapper and one raw reclaim site. Reclaim
#      consumes a non-forgeable authorization emitted only after close was
#      attempted and Emscripten socket deletion succeeded.
#
# Exit codes:
#   0 — no violations found
#   1 — one or more violations detected
#   2 — the guard could not do its job (canonical sources missing / scan error)

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

# Report one bare-bool field violation. Reads the enclosing scan loop's
# file/lineno/struct_name/line variables directly so callers never pass the
# file being read as an argument (shellcheck SC2094).
_flag_bare_bool() {
    echo -e "${RED}VIOLATION:${NC} ${file}:${lineno}: bare 'bool' in #[repr(C)] struct '${struct_name}'"
    echo "  ${line}"
    echo "  Use an explicit integer alias matching the upstream header (for example C_BOOL = u8 or EM_BOOL = c_int)."
    echo "  Bare Rust bool has no supported FFI ABI guarantee for repr(C) fields."
    VIOLATIONS=$((VIOLATIONS + 1))
}

# ── Scan roots ────────────────────────────────────────────────────────
# The same production roots check-no-panics.sh scans: FFI ABI mistakes
# anywhere in the workspace must be visible, not only in the core crate
# (the lockstep Godot adapter crate under crates/ is real FFI-adjacent
# code today).
SCAN_DIRS=()
for dir in src examples crates/*/src crates/*/examples tools/*/src; do
    if [ -d "$dir" ]; then
        SCAN_DIRS+=("$dir")
    fi
done
if [ ! -d src ]; then
    echo -e "${RED}FATAL: src/ is missing — the core crate's sources were not scanned.${NC}" >&2
    exit 2
fi
if [ "${#SCAN_DIRS[@]}" -eq 0 ]; then
    echo -e "${RED}FATAL: no production source roots found — nothing was scanned.${NC}" >&2
    exit 2
fi

FFI_SCAN_TMP="$(mktemp -d "${TMPDIR:-/tmp}/sf-ffi-scan.XXXXXX")"
trap 'rm -rf "$FFI_SCAN_TMP"' EXIT

echo -e "${YELLOW}=== FFI safety check ===${NC}"
echo ""

# ── Check 1: bool in #[repr(C)] structs ─────────────────────────────
# Extract all #[repr(C)] struct blocks and flag any that contain a bare
# `bool` field. The regex looks for `: bool` or `: bool,` inside struct
# bodies that follow a #[repr(C)] annotation.
echo -e "${YELLOW}Check 1: Scanning for bare 'bool' fields in #[repr(C)] structs...${NC}"

# Find all .rs files that contain #[repr(C)]. Grep exit 1 (no match) is
# normal; any other failure must fail the guard instead of reporting a
# vacuous pass.
REPR_C_FILES=""
grep_status=0
grep -rl '#\[repr(C)\]' "${SCAN_DIRS[@]}" 2>/dev/null >"${FFI_SCAN_TMP}/reprc" || grep_status=$?
if [ "$grep_status" -gt 1 ]; then
    echo -e "${RED}FATAL: scan for #[repr(C)] failed (grep exit $grep_status).${NC}" >&2
    exit 2
fi
REPR_C_FILES=$(cat "${FFI_SCAN_TMP}/reprc")

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
            if printf '%s\n' "$line" | grep -q '#\[repr(C)\]'; then
                in_repr_c=true
                continue
            fi

            # Detect struct opening after #[repr(C)]
            if [ "$in_repr_c" = true ] && printf '%s\n' "$line" | grep -qE '^[[:space:]]*(pub[[:space:]]+)?struct[[:space:]]+'; then
                in_struct=true
                struct_name=$(printf '%s\n' "$line" | grep -oE 'struct[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' | sed 's/struct[[:space:]]*//')
                # Count opening braces on this line
                opens=$(printf '%s\n' "$line" | tr -cd '{' | wc -c)
                closes=$(printf '%s\n' "$line" | tr -cd '}' | wc -c)
                brace_depth=$((brace_depth + opens - closes))
                in_repr_c=false
                # One-line struct bodies (`struct X { pub flag: bool }`)
                # never reach the per-line field scan below, so check this
                # declaration line directly whenever it opens a body.
                if printf '%s\n' "$line" | grep -q '{' && printf '%s\n' "$line" | grep -v '^[[:space:]]*//' | grep -qE ':[[:space:]]*bool[[:space:]]*[,}]?[[:space:]]*$'; then
                    _flag_bare_bool
                fi
                continue
            fi

            # If we hit something else after #[repr(C)], cancel it
            if [ "$in_repr_c" = true ]; then
                # Allow blank lines, attributes, and doc comments between #[repr(C)] and struct
                if printf '%s\n' "$line" | grep -qE '^[[:space:]]*$|^[[:space:]]*#\[|^[[:space:]]*///'; then
                    continue
                fi
                in_repr_c=false
            fi

            # Inside a #[repr(C)] struct body
            if [ "$in_struct" = true ]; then
                opens=$(printf '%s\n' "$line" | tr -cd '{' | wc -c)
                closes=$(printf '%s\n' "$line" | tr -cd '}' | wc -c)
                brace_depth=$((brace_depth + opens - closes))

                # Check for bare bool field: `: bool` not preceded by `//`
                if printf '%s\n' "$line" | grep -v '^[[:space:]]*//' | grep -qE ':[[:space:]]*bool[[:space:]]*[,}]?[[:space:]]*$'; then
                    _flag_bare_bool
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

FFI_FILES=""
grep_status=0
grep -rl 'emscripten_websocket_set_' "${SCAN_DIRS[@]}" 2>/dev/null >"${FFI_SCAN_TMP}/set_cb" || grep_status=$?
if [ "$grep_status" -gt 1 ]; then
    echo -e "${RED}FATAL: scan for FFI callback registrations failed (grep exit $grep_status).${NC}" >&2
    exit 2
fi
FFI_FILES=$(cat "${FFI_SCAN_TMP}/set_cb")

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
            | grep -v '^[[:space:]]*//' \
            | grep -v '//.*emscripten_websocket_set_' \
            | grep -v 'fn emscripten_websocket_set_' \
            | grep -v 'type.*emscripten_websocket_set_' \
            || true)

        # Read the file into an array so we can inspect context lines.
        # (No mapfile: bash 3.2 on macOS does not provide it.)
        file_lines=()
        while IFS= read -r _line || [ -n "$_line" ]; do
            file_lines+=("$_line")
        done <"$file"

        while IFS= read -r match_line; do
            match_line="${match_line//$'\r'/}"
            [ -z "$match_line" ] && continue

            lineno=$(printf '%s\n' "$match_line" | cut -d: -f1)
            code=$(printf '%s\n' "$match_line" | cut -d: -f2-)
            # Strip leading whitespace (pure bash, avoids SC2001)
            trimmed="${code#"${code%%[![:space:]]*}"}"

            # Skip lines where the call is clearly inside an expression:
            #   - Line contains `let ... =` before the call
            #   - Line contains `=` before the call (assignment)
            #   - Line contains `if ` before the call
            if printf '%s\n' "$code" | grep -qE '(let[[:space:]]+.*=|=[[:space:]]*|if[[:space:]]+).*emscripten_websocket_set_'; then
                continue
            fi

            # If the line starts with the FFI call, check the preceding
            # non-blank line. If it's part of a tuple/array expression
            # (e.g., ("name",) or [(...),]), the return value is captured.
            if printf '%s\n' "$trimmed" | grep -qE '^emscripten_websocket_set_'; then
                checked=false
                # Walk backwards to find the nearest non-blank, non-comment line.
                idx=$((lineno - 2)) # 0-indexed, minus one more for previous line
                while [ "$idx" -ge 0 ]; do
                    prev_line="${file_lines[$idx]}"
                    # Strip leading whitespace (pure bash, avoids SC2001)
                    prev_trimmed="${prev_line#"${prev_line%%[![:space:]]*}"}"
                    # Skip blank lines and comment-only lines
                    if [ -z "$prev_trimmed" ] || printf '%s\n' "$prev_trimmed" | grep -qE '^[[:space:]]*//'; then
                        idx=$((idx - 1))
                        continue
                    fi
                    # If the previous meaningful line ends with ( or , or =
                    # or contains "let", it means this call is inside an expression.
                    if printf '%s\n' "$prev_trimmed" | grep -qE '[,(=][[:space:]]*$'; then
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
    echo -e "${GREEN}  Check 2: PASS — all callback-registration return values are checked.${NC}"
fi
echo ""

# ── Check 3: Target guard for Emscripten FFI modules ─────────────────
# Files that declare or call Emscripten-specific FFI functions must contain
# a compile_error!() guard to prevent compilation on non-Emscripten targets.
echo -e "${YELLOW}Check 3: Scanning for missing target guards in Emscripten FFI modules...${NC}"

CHECK3_VIOLATIONS=0

EMSCRIPTEN_FFI_FILES=""
grep_status=0
grep -rl 'emscripten_websocket_new\|emscripten_websocket_set_' "${SCAN_DIRS[@]}" 2>/dev/null >"${FFI_SCAN_TMP}/emffi" || grep_status=$?
if [ "$grep_status" -gt 1 ]; then
    echo -e "${RED}FATAL: scan for Emscripten FFI files failed (grep exit $grep_status).${NC}" >&2
    exit 2
fi
EMSCRIPTEN_FFI_FILES=$(cat "${FFI_SCAN_TMP}/emffi")

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

EXTERN_C_FILES=""
grep_status=0
grep -rl 'extern "C" fn' "${SCAN_DIRS[@]}" 2>/dev/null >"${FFI_SCAN_TMP}/externc" || grep_status=$?
if [ "$grep_status" -gt 1 ]; then
    echo -e "${RED}FATAL: scan for extern \"C\" fn files failed (grep exit $grep_status).${NC}" >&2
    exit 2
fi
EXTERN_C_FILES=$(cat "${FFI_SCAN_TMP}/externc")

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
        # (No mapfile: bash 3.2 on macOS does not provide it.)
        file_lines=()
        while IFS= read -r _line || [ -n "$_line" ]; do
            file_lines+=("$_line")
        done <"$file"
        total_lines=${#file_lines[@]}

        for ((i = 0; i < total_lines; i++)); do
            line="${file_lines[$i]}"
            line="${line//$'\r'/}"

            # Skip lines inside extern "C" { } blocks (FFI declarations, not callback definitions).
            # We only care about standalone extern "C" fn definitions.
            if printf '%s\n' "$line" | grep -qE '^[[:space:]]*extern "C" fn '; then
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
                if ! printf '%s\n' "$prev_line" | grep -qE '^// SAFETY:'; then
                    fn_name=$(printf '%s\n' "$line" | grep -oE 'fn [A-Za-z_][A-Za-z0-9_]*' | sed 's/fn //')
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

# ── Check 5: transport close must also delete/unregister callbacks ────
printf '%b\n' "${YELLOW}Check 5: Scanning transport close methods for close/delete cleanup...${NC}"

CHECK5_VIOLATIONS=0
CLOSE_FILE='src/transports/emscripten_websocket.rs'
CLOSE_LINE='        let close_error = self.close_native_socket().err();'
DELETE_LINE='        let delete_result = self.delete_after_close_attempt();'

if [ ! -f "$CLOSE_FILE" ]; then
    printf '%b\n' "${GREEN}  Emscripten transport absent — nothing to check.${NC}"
else
    close_matches=$(grep -nFx "$CLOSE_LINE" "$CLOSE_FILE" || true)
    delete_matches=$(grep -nFx "$DELETE_LINE" "$CLOSE_FILE" || true)
    poll_start=$(grep -nE '^[[:space:]]*fn poll_close\(' "$CLOSE_FILE" | cut -d: -f1 | head -1)
    poll_end=$(grep -nE '^[[:space:]]*fn is_ready\(' "$CLOSE_FILE" | cut -d: -f1 | head -1)
    close_count=$(printf '%s\n' "$close_matches" | grep -c . || true)
    delete_count=$(printf '%s\n' "$delete_matches" | grep -c . || true)
    if [ "$close_count" -ne 1 ] || [ "$delete_count" -ne 1 ]; then
        printf '%b\n' "${RED}VIOLATION:${NC} poll_close must contain one unconditional top-level close and one unconditional top-level delete"
        CHECK5_VIOLATIONS=$((CHECK5_VIOLATIONS + 1))
    else
        close_lineno=${close_matches%%:*}
        delete_lineno=${delete_matches%%:*}
        poll_block=$(sed -n "${poll_start},${poll_end}p" "$CLOSE_FILE")
        cleanup_gap=""
        if [ "$delete_lineno" -gt $((close_lineno + 1)) ]; then
            cleanup_gap=$(sed -n "$((close_lineno + 1)),$((delete_lineno - 1))p" "$CLOSE_FILE")
        fi
        multiline_string=false
        if printf '%s\n' "$poll_block" | awk '
            {
                line = $0
                gsub(/\\\"/, "", line)
                if (gsub(/\"/, "", line) % 2 == 1) found = 1
            }
            END { exit found ? 0 : 1 }
        '; then
            multiline_string=true
        fi
        if [ -z "$poll_start" ] || [ -z "$poll_end" ] ||
            [ "$close_lineno" -le "$poll_start" ] || [ "$delete_lineno" -ge "$poll_end" ] ||
            [ "$close_lineno" -ge "$delete_lineno" ] || [ -n "$cleanup_gap" ] ||
            "$multiline_string" || printf '%s\n' "$poll_block" | grep -Eq 'r#*"|/\*|\*/'; then
            printf '%b\n' "${RED}VIOLATION:${NC} poll_close must attempt native close before callback deletion"
            CHECK5_VIOLATIONS=$((CHECK5_VIOLATIONS + 1))
        fi
    fi
fi

VIOLATIONS=$((VIOLATIONS + CHECK5_VIOLATIONS))

if [ "$CHECK5_VIOLATIONS" -eq 0 ]; then
    printf '%b\n' "${GREEN}  Check 5: PASS — all transport close methods that close also delete/unregister.${NC}"
fi
printf '\n'

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

# ── Check 7: callback reclamation requires typed authorization ───────
# The owning wrapper consumes a private capability emitted by CleanupState.
printf '%b\n' "${YELLOW}Check 7: Scanning callback reclamation capability boundary...${NC}"

CHECK7_VIOLATIONS=0
EXPECTED_RECLAIM='unsafe { drop(Box::from_raw(state_ptr.as_ptr())) };'
EXPECTED_FILE='src/transports/emscripten_websocket.rs'
EXPECTED_COUNT=0
RECLAIM_FOUND=0

grep_status=0
grep -rnE 'from_raw(_parts)?|drop_in_place|std::alloc::dealloc|std::mem::transmute|(^|[^[:alnum:]_])(dealloc|free|transmute)[[:space:]]*\(' "${SCAN_DIRS[@]}" 2>/dev/null >"${FFI_SCAN_TMP}/reclaim" || grep_status=$?
if [ "$grep_status" -gt 1 ]; then
    printf '%b\n' "${RED}FATAL: scan for raw ownership reclamation failed (grep exit $grep_status).${NC}" >&2
    exit 2
fi
while IFS=: read -r file lineno line; do
    [ -n "$file" ] || continue
    trimmed=$(printf '%s\n' "$line" | sed 's/^[[:space:]]*//')
    case "$trimmed" in
        '//'*) continue ;;
    esac
    # Remove only known borrowed/framing constructors, then audit any other
    # ownership-looking token that remains on the same line. The tungstenite
    # exception is intentionally limited to its exact crate-qualified path at
    # the start of the trimmed line, so namespace lookalikes remain audited.
    audited_line=$(printf '%s\n' "$line" |
        sed -E 's/std::slice::from_raw_parts[[:space:]]*\(//g')
    if printf '%s\n' "$trimmed" | grep -qE '^(let[[:space:]]+[[:alnum:]_]+[[:space:]]*=[[:space:]]*)?tokio_tungstenite::WebSocketStream::from_raw_socket[[:space:]]*\('; then
        audited_line=$(printf '%s\n' "$audited_line" |
            sed -E 's/tokio_tungstenite::WebSocketStream::from_raw_socket[[:space:]]*\(//')
    fi
    if ! printf '%s\n' "$audited_line" | grep -qE 'from_raw(_parts)?|drop_in_place|std::alloc::dealloc|std::mem::transmute|(^|[^[:alnum:]_])(dealloc|free|transmute)[[:space:]]*\('; then
        continue
    fi
    RECLAIM_FOUND=1
    if [ "$file" = "$EXPECTED_FILE" ] && [ "$trimmed" = "$EXPECTED_RECLAIM" ]; then
        EXPECTED_COUNT=$((EXPECTED_COUNT + 1))
        continue
    fi
    printf '%b\n' "${RED}VIOLATION:${NC} $file:$lineno: raw ownership reclamation bypasses RegisteredCallbackState authorization"
    printf '  %s\n' "$line"
    CHECK7_VIOLATIONS=$((CHECK7_VIOLATIONS + 1))
done <"${FFI_SCAN_TMP}/reclaim"

if [ "$RECLAIM_FOUND" -eq 1 ] && [ "$EXPECTED_COUNT" -ne 1 ]; then
    printf '%b\n' "${RED}VIOLATION:${NC} expected exactly one authorized callback-state reclaim, found $EXPECTED_COUNT"
    CHECK7_VIOLATIONS=$((CHECK7_VIOLATIONS + 1))
fi

if [ "$RECLAIM_FOUND" -eq 1 ] &&
    { ! grep -qF 'fn reclaim(&mut self, _authorization: ReclaimAuthorization)' "$EXPECTED_FILE" ||
        ! grep -qF 'let Some(state_ptr) = self.0.take()' "$EXPECTED_FILE"; }; then
    printf '%b\n' "${RED}VIOLATION:${NC} RegisteredCallbackState::reclaim must consume authorization and take ownership exactly once"
    CHECK7_VIOLATIONS=$((CHECK7_VIOLATIONS + 1))
fi

aliased_status=0
grep -rnE '(dealloc|free|from_raw|transmute)[[:space:]]+as[[:space:]]+' "${SCAN_DIRS[@]}" >/dev/null 2>&1 || aliased_status=$?
if [ "$aliased_status" -gt 1 ]; then
    printf '%b\n' "${RED}FATAL: scan for aliased reclamation functions failed (grep exit $aliased_status).${NC}" >&2
    exit 2
fi
if [ "$aliased_status" -eq 0 ]; then
    printf '%b\n' "${RED}VIOLATION:${NC} raw reclamation functions must not be aliased"
    CHECK7_VIOLATIONS=$((CHECK7_VIOLATIONS + 1))
fi

VIOLATIONS=$((VIOLATIONS + CHECK7_VIOLATIONS))

if [ "$CHECK7_VIOLATIONS" -eq 0 ]; then
    printf '%b\n' "${GREEN}  Check 7: PASS — callback state has one typed, exactly-once reclamation boundary.${NC}"
fi
printf '\n'

# ── Result ────────────────────────────────────────────────────────────
if [ "$VIOLATIONS" -gt 0 ]; then
    echo -e "${RED}FAILED: $VIOLATIONS FFI safety violation(s) found.${NC}"
    echo "Fix all violations before committing."
    exit 1
else
    echo -e "${GREEN}PASSED: No FFI safety issues found.${NC}"
    exit 0
fi
