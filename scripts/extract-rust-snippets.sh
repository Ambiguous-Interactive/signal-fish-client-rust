#!/usr/bin/env bash
# extract-rust-snippets.sh — Extract Rust code blocks from markdown files and
# verify they compile.
#
# Scans docs/, README.md, and .llm/context.md for ```rust code blocks, wraps
# them in appropriate scaffolding, and runs `cargo check` on each snippet.
#
# Snippets that contain placeholder markers (`...`, `// ...`, `/* ... */`) or
# are clearly incomplete fragments (bare function signatures, method calls
# without context) are skipped automatically.
#
# Exit codes:
#   0 — all snippets compile (or none found)
#   1 — one or more snippets failed to compile

set -euo pipefail

# ── Resolve paths relative to this script ────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Color constants ──────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# ── Counters ─────────────────────────────────────────────────────────
TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0

# ── Collect markdown files ───────────────────────────────────────────
MD_FILES=()

# docs/ directory
if [ -d "$REPO_ROOT/docs" ]; then
    while IFS= read -r f; do
        f="${f//$'\r'/}"
        MD_FILES+=("$f")
    done < <(find "$REPO_ROOT/docs" -name '*.md' -type f | sort)
fi

# README.md
if [ -f "$REPO_ROOT/README.md" ]; then
    MD_FILES+=("$REPO_ROOT/README.md")
fi

# .llm/context.md
if [ -f "$REPO_ROOT/.llm/context.md" ]; then
    MD_FILES+=("$REPO_ROOT/.llm/context.md")
fi

if [ ${#MD_FILES[@]} -eq 0 ]; then
    echo -e "${YELLOW}No markdown files found to scan.${NC}"
    exit 0
fi

echo -e "${YELLOW}=== Rust snippet extraction & compilation check ===${NC}"
echo "Scanning ${#MD_FILES[@]} markdown file(s)..."
echo ""

# ── Create temp project ──────────────────────────────────────────────
TEMP_DIR=""
# shellcheck disable=SC2317
cleanup() {
    if [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ]; then
        rm -rf "$TEMP_DIR"
    fi
}
trap cleanup EXIT

TEMP_DIR="$(mktemp -d)"
TEMP_SRC="$TEMP_DIR/src"
mkdir -p "$TEMP_SRC"

# Create Cargo.toml for the temp project.
# The path dependency points back to the real crate.
cat > "$TEMP_DIR/Cargo.toml" << 'TOML'
[package]
name = "snippet-check"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
signal-fish-client = { path = "REPO_ROOT_PLACEHOLDER", features = ["transport-websocket"] }
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
uuid = { version = "1", features = ["v4", "serde"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "2.0"
futures-util = "0.3"
TOML

# Replace the placeholder with the real repo root path.
sed -i "s|REPO_ROOT_PLACEHOLDER|$REPO_ROOT|g" "$TEMP_DIR/Cargo.toml"

# Create a dummy lib.rs so cargo doesn't complain.
echo "" > "$TEMP_SRC/lib.rs"

# ── Extract and check snippets ───────────────────────────────────────

# should_skip — returns 0 (true) if the snippet should be skipped.
should_skip() {
    local content="$1"

    # Placeholder patterns — incomplete fragments.
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*\.\.\.[[:space:]]*$'; then return 0; fi
    if printf '%s\n' "$content" | grep -qE '//[[:space:]]*\.\.\.' ; then return 0; fi
    if printf '%s\n' "$content" | grep -qE '/\*[[:space:]]*\.\.\.[[:space:]]*\*/' ; then return 0; fi
    if printf '%s\n' "$content" | grep -qF '…'; then return 0; fi

    # Bare function/method signatures without bodies (API reference docs).
    # e.g. "fn join_room(&self, params: JoinRoomParams) -> Result<()>"
    # These start with fn/async fn and end with a return type but no {.
    local trimmed
    trimmed="$(printf '%s\n' "$content" | tr -d '[:space:]')"
    if printf '%s\n' "$trimmed" | grep -qE '^(async)?fn[^{]+$'; then return 0; fi

    # Snippets that are a bare #[serde(...)] attribute without a struct/enum.
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*#\[serde'; then
        if ! printf '%s\n' "$content" | grep -qE '(struct|enum|fn|impl|trait)'; then
            return 0
        fi
    fi

    # Pseudo-code method signature listings (e.g. "client.join_room(...) -> Result<()>").
    # These have `-> Result` on lines that start with a variable reference, not fn def.
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*[[:alnum:]_]+\.[[:alnum:]_]+\(.*\)[[:space:]]*->[[:space:]]*Result'; then
        return 0
    fi

    # Snippets that impl a trait for a type that isn't defined in the snippet
    # (e.g. "impl Transport for LoopbackTransport" without struct definition).
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*impl[[:space:]]+[[:alnum:]_]+[[:space:]]+for[[:space:]]+[[:alnum:]_]+'; then
        local impl_type
        impl_type="$(printf '%s\n' "$content" | grep -oE 'impl[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]+for[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' | head -1 | sed 's/.*for[[:space:]]*//')"
        if [ -n "$impl_type" ] && ! printf '%s\n' "$content" | grep -qE "(struct|enum)[[:space:]]+${impl_type}"; then
            return 0
        fi
    fi

    # Very short snippets (1-3 non-blank lines) referencing undefined variables
    # like `transport`, `config`, `url`, etc. from surrounding context.
    local nonblank_count
    nonblank_count="$(printf '%s\n' "$content" | grep -c -E '^[[:space:]]*[^[:space:]]' || true)"
    if [ "$nonblank_count" -le 3 ]; then
        # If it references variables from surrounding doc context, skip.
        if printf '%s\n' "$content" | grep -qwE '(transport|config|stream|my_stream|url)'; then
            if ! printf '%s\n' "$content" | grep -qE '^[[:space:]]*(fn|struct|enum|trait|impl|pub)[[:space:]]'; then
                return 0
            fi
        fi
    fi

    # Snippets that reference types not defined in the snippet and not from the
    # crate (e.g. LoopbackTransport from a prior example snippet).
    if printf '%s\n' "$content" | grep -qF 'LoopbackTransport'; then
        if ! printf '%s\n' "$content" | grep -qE 'struct[[:space:]]+LoopbackTransport'; then
            return 0
        fi
    fi

    # Snippets that reference undefined variables like player_id, room_id,
    # auth_token without defining them.
    if printf '%s\n' "$content" | grep -qE '(^|[^[:alnum:]_])client\.reconnect\('; then
        if ! printf '%s\n' "$content" | grep -qE '^[[:space:]]*let[[:space:]]+(player_id|room_id|auth_token)([^[:alnum:]_]|$)'; then
            return 0
        fi
    fi

    return 1
}

# wrap_snippet — wraps a snippet in appropriate scaffolding and writes it
# to a .rs file. Returns 0 if the file was written, 1 if skipped.
wrap_snippet() {
    local content="$1"
    local out_file="$2"

    local allows='#![allow(unused, dead_code, unused_imports, unused_variables, unreachable_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::needless_return, clippy::let_and_return, clippy::redundant_closure, clippy::todo)]'

    local prelude
    prelude="use signal_fish_client::*;
use signal_fish_client::protocol::*;
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use std::time::Duration;"

    # If snippet contains `fn main`, it is a complete program.
    if printf '%s\n' "$content" | grep -q 'fn main'; then
        printf '%s\n%s\n' "$allows" "$content" > "$out_file"
        return 0
    fi

    # Check if the snippet defines its own function or async fn (not as a method).
    local has_fn_def=false
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[[:alnum:]_]+'; then
        has_fn_def=true
    fi

    # Check if snippet defines a struct/enum/trait/impl/type.
    local has_item_def=false
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*(pub[[:space:]]+)?(struct|enum|trait|impl|type|mod|const|static)[[:space:]]'; then
        has_item_def=true
    fi

    # Check if snippet has an #[async_trait] or #[derive()] attribute.
    local has_attr=false
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*#\['; then
        has_attr=true
    fi

    # Check if snippet has use statements.
    local has_use=false
    if printf '%s\n' "$content" | grep -qE '^[[:space:]]*use[[:space:]]'; then
        has_use=true
    fi

    # Check if snippet uses variables that need to be in scope.
    local needs_client=false
    local needs_event=false
    if printf '%s\n' "$content" | grep -qE '(^|[^[:alnum:]_])client\.'; then
        needs_client=true
    fi
    if printf '%s\n' "$content" | grep -qE '(^|[^[:alnum:]_])match[[:space:]]+event([^[:alnum:]_]|$)'; then
        needs_event=true
    fi
    if printf '%s\n' "$content" | grep -qwE 'event' && [ "$needs_event" = false ]; then
        # References to event outside match — might be a function parameter.
        if printf '%s\n' "$content" | grep -qE '(fn[[:space:]]+[[:alnum:]_]+.*event)'; then
            needs_event=false  # It's a function parameter, fine.
        fi
    fi

    # Bare struct/enum definitions without derives from protocol docs — skip.
    if [ "$has_item_def" = true ] && [ "$has_fn_def" = false ]; then
        local first_nonblank
        first_nonblank="$(printf '%s\n' "$content" | grep -m1 -E '^[[:space:]]*[^[:space:]]' || true)"
        if printf '%s\n' "$first_nonblank" | grep -qE '^[[:space:]]*pub[[:space:]]+(struct|enum)[[:space:]]'; then
            if ! printf '%s\n' "$content" | grep -q '#\[derive'; then
                return 1  # Skip — bare type definition listing.
            fi
        fi
    fi

    # ── Strategy 1: Complete items (fn defs, impls, traits) ──────────
    # Snippets that define standalone functions, impls, or traits.
    if [ "$has_fn_def" = true ] || [ "$has_item_def" = true ] || [ "$has_attr" = true ]; then
        # These are top-level items — they don't need fn main wrapping.
        # But they may need use/prelude.
        {
            printf '%s\n' "$allows"
            if [ "$has_use" = false ]; then
                printf '%s\n' "$prelude"
            else
                # Snippet has its own use statements. Add only the ones
                # it doesn't already have.
                if ! printf '%s\n' "$content" | grep -qE '^[[:space:]]*use async_trait'; then
                    echo "use async_trait::async_trait;"
                fi
                if ! printf '%s\n' "$content" | grep -qE '^[[:space:]]*use serde'; then
                    echo "use serde::{Serialize, Deserialize};"
                fi
                if ! printf '%s\n' "$content" | grep -qE '^[[:space:]]*use std::time'; then
                    echo "use std::time::Duration;"
                fi
                # Always add the wildcard imports for types the snippet
                # may reference indirectly.
                if ! printf '%s\n' "$content" | grep -qE 'use signal_fish_client::\*'; then
                    echo "use signal_fish_client::*;"
                    echo "use signal_fish_client::protocol::*;"
                fi
            fi
            printf '%s\n' "$content"
        } > "$out_file"
        return 0
    fi

    # ── Strategy 2: Body code needing fn main wrapping ───────────────
    # Snippets that are expressions, let bindings, match arms, etc.

    # Determine if the wrapper function needs to return Result (uses `?`).
    local uses_question_mark=false
    if printf '%s\n' "$content" | grep -qE '\?[[:space:]]*;'; then
        uses_question_mark=true
    fi

    # Determine if the wrapper function needs to be async (uses `.await`).
    local uses_await=false
    if printf '%s\n' "$content" | grep -qF '.await'; then
        uses_await=true
    fi

    {
        printf '%s\n' "$allows"
        # Add prelude imports. We always add the full prelude and let
        # the #![allow(unused_imports)] suppress warnings. This avoids
        # breaking multi-line `use` blocks by trying to split them out.
        printf '%s\n' "$prelude"

        # Build the function signature.
        local fn_sig="fn _snippet_main()"
        if [ "$uses_await" = true ]; then
            fn_sig="async fn _snippet_main()"
        fi
        if [ "$uses_question_mark" = true ]; then
            fn_sig="${fn_sig} -> std::result::Result<(), Box<dyn std::error::Error>>"
        fi
        printf '%s\n' "${fn_sig} {"

        # Inject dummy variables that the snippet needs.
        if [ "$needs_client" = true ]; then
            echo "let client: SignalFishClient = todo!();"
        fi
        if [ "$needs_event" = true ]; then
            echo "let event: SignalFishEvent = todo!();"
        fi

        printf '%s\n' "$content"

        if [ "$uses_question_mark" = true ]; then
            echo "Ok(())"
        fi
        echo "}"
    } > "$out_file"
    return 0
}

# Process each markdown file.
for md_file in "${MD_FILES[@]}"; do
    relative="${md_file#"$REPO_ROOT/"}"
    in_rust_block=false
    block_content=""
    block_start_line=0
    block_lang=""

    line_num=0
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line//$'\r'/}"
        line_num=$((line_num + 1))

        if [ "$in_rust_block" = false ]; then
            # Detect start of a fenced code block.
            if printf '%s\n' "$line" | grep -qE '^[[:space:]]*```'; then
                # Extract the language tag.
                block_lang="$(printf '%s\n' "$line" | sed -E 's/^[[:space:]]*```[[:space:]]*//' | sed -E 's/[[:space:]].*$//')"
                # Only process rust blocks.
                case "$block_lang" in
                    rust|rust,no_run)
                        in_rust_block=true
                        block_content=""
                        block_start_line=$line_num
                        ;;
                    rust,ignore)
                        # rust,ignore blocks are intentionally not compiled
                        # (e.g., platform-specific or external-crate snippets).
                        in_rust_block=true
                        block_content=""
                        block_start_line=$line_num
                        block_lang="rust,ignore"
                        ;;
                esac
            fi
        else
            # Check for end of code block.
            if printf '%s\n' "$line" | grep -qE '^[[:space:]]*```[[:space:]]*$'; then
                in_rust_block=false
                TOTAL=$((TOTAL + 1))

                # Skip rust,ignore blocks — they are explicitly marked as
                # not compilable (e.g., platform-specific or external deps).
                if [ "$block_lang" = "rust,ignore" ]; then
                    SKIPPED=$((SKIPPED + 1))
                    echo -e "${YELLOW}SKIP${NC} $relative:$block_start_line (rust,ignore)"
                    continue
                fi

                # Skip empty blocks.
                if [ -z "$(printf '%s\n' "$block_content" | tr -d '[:space:]')" ]; then
                    SKIPPED=$((SKIPPED + 1))
                    continue
                fi

                # Skip fragments and bare snippets.
                if should_skip "$block_content"; then
                    SKIPPED=$((SKIPPED + 1))
                    echo -e "${YELLOW}SKIP${NC} $relative:$block_start_line (fragment or placeholder)"
                    continue
                fi

                # Write snippet to a file.
                snippet_file="$TEMP_SRC/snippet_${TOTAL}.rs"
                if ! wrap_snippet "$block_content" "$snippet_file"; then
                    SKIPPED=$((SKIPPED + 1))
                    echo -e "${YELLOW}SKIP${NC} $relative:$block_start_line (bare type definition)"
                    continue
                fi

                # Update lib.rs to include this snippet as a module.
                mod_name="snippet_${TOTAL}"
                echo "#[path = \"${mod_name}.rs\"] mod ${mod_name};" >> "$TEMP_SRC/lib.rs"
            else
                block_content="${block_content}${line}
"
            fi
        fi
    done < "$md_file"
done

if [ "$TOTAL" -eq 0 ]; then
    echo -e "${GREEN}No Rust snippets found in markdown files.${NC}"
    exit 0
fi

echo ""
echo "Found $TOTAL Rust snippet(s), skipped $SKIPPED."
echo ""

# ── Compile all snippets at once ─────────────────────────────────────
echo -e "${YELLOW}Running cargo check on extracted snippets...${NC}"

COMPILABLE=$((TOTAL - SKIPPED))
if [ "$COMPILABLE" -eq 0 ]; then
    echo -e "${GREEN}All snippets were skipped (fragments). Nothing to compile.${NC}"
    exit 0
fi

# Run cargo check in the temp directory.
if cargo check --manifest-path "$TEMP_DIR/Cargo.toml" 2>&1; then
    PASSED=$COMPILABLE
    echo ""
    echo -e "${GREEN}All $PASSED snippet(s) compiled successfully.${NC}"
else
    # Cargo check failed — try individual snippets to identify which ones fail.
    echo ""
    echo -e "${YELLOW}Batch check failed. Testing snippets individually...${NC}"
    echo ""

    for snippet_file in "$TEMP_SRC"/snippet_*.rs; do
        [ -f "$snippet_file" ] || continue
        mod_name="$(basename "$snippet_file" .rs)"

        # Create a temporary lib.rs with just this one module.
        echo "#[path = \"${mod_name}.rs\"] mod ${mod_name};" > "$TEMP_SRC/lib.rs"

        if cargo check --manifest-path "$TEMP_DIR/Cargo.toml" 2>/dev/null; then
            PASSED=$((PASSED + 1))
            echo -e "${GREEN}PASS${NC} $mod_name"
        else
            FAILED=$((FAILED + 1))
            echo -e "${RED}FAIL${NC} $mod_name"
            # Show the snippet content for debugging.
            echo "  --- snippet content ---"
            sed 's/^/    /' "$snippet_file"
            echo "  --- end snippet ---"
            echo ""
        fi
    done
fi

# ── Result ───────────────────────────────────────────────────────────
echo ""
echo "Results: $PASSED passed, $FAILED failed, $SKIPPED skipped (of $TOTAL total)"

if [ "$FAILED" -gt 0 ]; then
    echo -e "${RED}FAILED: $FAILED snippet(s) did not compile.${NC}"
    exit 1
else
    echo -e "${GREEN}PASSED: All extracted Rust snippets compile.${NC}"
    exit 0
fi
