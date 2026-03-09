#!/usr/bin/env bash
# check-docs-rendering.sh — Validate MkDocs documentation renders correctly.
#
# Catches rendering issues that silently break the published site:
#   - Pygments-incompatible code fence language tags (e.g., rust,ignore)
#   - Mermaid diagrams that render as plain text
#   - Code blocks misclassified as "language-text" that contain Rust code
#   - Broken code fences that leak as raw markdown
#   - Unclosed or malformed code fences in source
#   - Missing MkDocs configuration (hooks, extensions, custom fences)
#
# Usage:
#   bash scripts/check-docs-rendering.sh
#   MKDOCS=path/to/mkdocs bash scripts/check-docs-rendering.sh
#
# Environment:
#   MKDOCS — path to the mkdocs binary (default: "mkdocs" from PATH)
#
# Exit codes:
#   0 — all checks passed
#   1 — one or more checks failed

set -euo pipefail

# ── Resolve paths ─────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Color constants ───────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
NC='\033[0m'

# ── Resolve mkdocs binary ────────────────────────────────────────
MKDOCS="${MKDOCS:-}"
if [ -z "$MKDOCS" ]; then
    if command -v mkdocs &>/dev/null; then
        MKDOCS="mkdocs"
    elif [ -x "/tmp/docs-venv/bin/mkdocs" ]; then
        MKDOCS="/tmp/docs-venv/bin/mkdocs"
    else
        echo -e "${RED}ERROR: mkdocs not found in PATH and /tmp/docs-venv/bin/mkdocs does not exist.${NC}" >&2
        echo "  Install: pip install mkdocs-material" >&2
        echo "  Or set: MKDOCS=/path/to/mkdocs" >&2
        exit 1
    fi
else
    # Validate user-provided MKDOCS path
    if [[ "$MKDOCS" == /* ]]; then
        # Absolute path — check that it exists and is executable
        if [ ! -x "$MKDOCS" ]; then
            echo -e "${RED}ERROR: MKDOCS is set to '$MKDOCS' but it is not executable or does not exist.${NC}" >&2
            echo "  Install: pip install mkdocs-material" >&2
            echo "  Or set: MKDOCS=/path/to/mkdocs" >&2
            exit 1
        fi
    else
        # Bare command name — check that it's in PATH
        if ! command -v "$MKDOCS" &>/dev/null; then
            echo -e "${RED}ERROR: MKDOCS is set to '$MKDOCS' but it was not found in PATH.${NC}" >&2
            echo "  Install: pip install mkdocs-material" >&2
            echo "  Or set: MKDOCS=/path/to/mkdocs" >&2
            exit 1
        fi
    fi
fi

# ── Phase tracking ───────────────────────────────────────────────
FAILURES=0
WARNINGS=0
CHECKS_PASSED=0
TOTAL_CHECKS=0

pass() {
    local msg="$1"
    CHECKS_PASSED=$((CHECKS_PASSED + 1))
    TOTAL_CHECKS=$((TOTAL_CHECKS + 1))
    echo -e "  ${GREEN}PASS${NC}  $msg"
}

fail() {
    local msg="$1"
    FAILURES=$((FAILURES + 1))
    TOTAL_CHECKS=$((TOTAL_CHECKS + 1))
    echo -e "  ${RED}FAIL${NC}  $msg"
}

warn() {
    local msg="$1"
    WARNINGS=$((WARNINGS + 1))
    TOTAL_CHECKS=$((TOTAL_CHECKS + 1))
    echo -e "  ${YELLOW}WARN${NC}  $msg"
}

info() {
    local msg="$1"
    echo -e "  ${YELLOW}INFO${NC}  $msg"
}

section() {
    echo ""
    echo -e "${BOLD}── $1 ──${NC}"
}

echo -e "${BOLD}${YELLOW}=== check-docs-rendering: MkDocs rendering validation ===${NC}"

# ══════════════════════════════════════════════════════════════════
# Phase 1: Config validation
# ══════════════════════════════════════════════════════════════════
section "Phase 1: MkDocs configuration validation"

MKDOCS_YML="$REPO_ROOT/mkdocs.yml"
if [ ! -f "$MKDOCS_YML" ]; then
    fail "mkdocs.yml not found at $MKDOCS_YML"
else
    # 1a. Check for rustdoc_codeblocks hook
    if grep -qE '^[[:space:]]*-[[:space:]]+hooks/rustdoc_codeblocks\.py' "$MKDOCS_YML"; then
        pass "rustdoc_codeblocks hook is configured"
    else
        fail "rustdoc_codeblocks hook is NOT configured in mkdocs.yml"
        info "Add under 'hooks:' section: - hooks/rustdoc_codeblocks.py"
    fi

    # 1b. Check for pymdownx.superfences extension
    if grep -qE '^[[:space:]]*-[[:space:]]+pymdownx\.superfences' "$MKDOCS_YML"; then
        pass "pymdownx.superfences extension is enabled"
    else
        fail "pymdownx.superfences extension is NOT enabled in mkdocs.yml"
        info "Add to 'markdown_extensions:': - pymdownx.superfences"
    fi

    # 1c. Check for mermaid custom_fences configuration
    MERMAID_NAME=false
    MERMAID_CLASS=false
    if grep -qE '^[[:space:]]*-[[:space:]]*name:[[:space:]]*mermaid' "$MKDOCS_YML"; then
        MERMAID_NAME=true
    fi
    if grep -qE '^[[:space:]]*class:[[:space:]]*mermaid' "$MKDOCS_YML"; then
        MERMAID_CLASS=true
    fi
    if [ "$MERMAID_NAME" = true ] && [ "$MERMAID_CLASS" = true ]; then
        pass "Mermaid custom_fences configured (name: mermaid, class: mermaid)"
    else
        fail "Mermaid custom_fences configuration is incomplete in mkdocs.yml"
        info "Ensure custom_fences under pymdownx.superfences has: name: mermaid, class: mermaid"
    fi

    # 1d. Check for pymdownx.highlight (needed for Pygments code highlighting)
    if grep -qE '^[[:space:]]*-[[:space:]]+pymdownx\.highlight' "$MKDOCS_YML"; then
        pass "pymdownx.highlight extension is enabled"
    else
        warn "pymdownx.highlight extension not found — code blocks may not be highlighted"
    fi

    # 1e. Verify the hook file actually exists
    HOOK_FILE="$REPO_ROOT/hooks/rustdoc_codeblocks.py"
    if [ -f "$HOOK_FILE" ]; then
        pass "Hook file hooks/rustdoc_codeblocks.py exists"
    else
        fail "Hook file hooks/rustdoc_codeblocks.py is referenced in mkdocs.yml but does not exist"
    fi
fi

# ══════════════════════════════════════════════════════════════════
# Phase 2: Source markdown validation
# ══════════════════════════════════════════════════════════════════
section "Phase 2: Source markdown validation (docs/)"

DOCS_DIR="$REPO_ROOT/docs"
if [ ! -d "$DOCS_DIR" ]; then
    fail "docs/ directory does not exist"
else
    # Collect all markdown files under docs/ (exclude includes/ abbreviations)
    mapfile -t MD_FILES < <(find "$DOCS_DIR" -name '*.md' -type f ! -path '*/includes/*' | sort)

    if [ ${#MD_FILES[@]} -eq 0 ]; then
        fail "No markdown files found in docs/"
    else
        info "Found ${#MD_FILES[@]} markdown file(s) in docs/"

        # 2a. Check for unclosed code fences
        UNCLOSED_FOUND=false
        for md_file in "${MD_FILES[@]}"; do
            rel_path="${md_file#"$REPO_ROOT"/}"
            # Count opening and closing fences (lines starting with ```)
            # A file should have an even number of fence lines.
            fence_count=$(grep -cE '^[[:space:]]*```' "$md_file" || true)
            if [ $((fence_count % 2)) -ne 0 ]; then
                fail "Unclosed code fence in $rel_path (odd number of fence markers: $fence_count)"
                UNCLOSED_FOUND=true
            fi
        done
        if [ "$UNCLOSED_FOUND" = false ]; then
            pass "No unclosed code fences detected"
        fi

        # 2b. Check for well-formed mermaid blocks
        MERMAID_ISSUES=false
        for md_file in "${MD_FILES[@]}"; do
            rel_path="${md_file#"$REPO_ROOT"/}"
            # Extract line numbers of mermaid open fences
            mapfile -t MERMAID_OPENS < <(grep -nE '^[[:space:]]*```[[:space:]]*mermaid[[:space:]]*$' "$md_file" | cut -d: -f1)
            for line_no in "${MERMAID_OPENS[@]}"; do
                [ -z "$line_no" ] && continue
                # Find the next closing fence after this line
                tail_content=$(tail -n +"$((line_no + 1))" "$md_file")
                close_line=$(echo "$tail_content" | grep -nE '^[[:space:]]*```[[:space:]]*$' | head -1 | cut -d: -f1)
                if [ -z "$close_line" ]; then
                    fail "Unclosed mermaid block at $rel_path:$line_no"
                    MERMAID_ISSUES=true
                else
                    # Check that there is actual mermaid content inside
                    block_length=$((close_line - 1))
                    if [ "$block_length" -le 0 ]; then
                        warn "Empty mermaid block at $rel_path:$line_no"
                        MERMAID_ISSUES=true
                    fi
                fi
            done
        done
        if [ "$MERMAID_ISSUES" = false ]; then
            pass "All mermaid blocks are well-formed"
        fi

        # 2c. Check for code fence language tags Pygments won't recognize
        # The hook handles rust,* patterns, but warn about any OTHER unknown
        # language tags that could cause issues.
        KNOWN_LANGS="rust|python|py|bash|sh|shell|zsh|toml|yaml|yml|json|javascript|js|typescript|ts|html|css|xml|sql|c|cpp|go|java|kotlin|swift|ruby|text|console|diff|makefile|dockerfile|graphql|markdown|md|plaintext|ini|cfg|conf|http|csv|latex|tex|r|lua|perl|php|scala|haskell|hs|elixir|erlang|clojure|csharp|cs|fsharp|powershell|ps1|vim|nix|protobuf|proto|cmake|zig|ocaml|lisp|scheme|wasm|wat|asm|nasm|objc|groovy"
        UNKNOWN_LANGS_FOUND=false
        for md_file in "${MD_FILES[@]}"; do
            rel_path="${md_file#"$REPO_ROOT"/}"
            # Extract language tags from code fences (```lang)
            mapfile -t LANG_TAGS < <(sed -nE 's/^[[:space:]]*```[[:space:]]*([a-zA-Z0-9_,.-]+).*/\1/p' "$md_file" 2>/dev/null | sort -u)
            for tag in "${LANG_TAGS[@]}"; do
                [ -z "$tag" ] && continue
                # Skip mermaid — handled by custom_fences
                if [ "$tag" = "mermaid" ]; then
                    continue
                fi
                # rust,ignore etc. — handled by the hook, not a problem
                if [[ "$tag" =~ ^rust,[a-zA-Z_][a-zA-Z0-9_]*(,[a-zA-Z_][a-zA-Z0-9_]*)*$ ]]; then
                    continue
                fi
                # Check against known languages (case-insensitive)
                if ! echo "$tag" | grep -qiE "^($KNOWN_LANGS)$"; then
                    warn "Potentially unrecognized language tag '$tag' in $rel_path (may render as plain text)"
                    UNKNOWN_LANGS_FOUND=true
                fi
            done
        done
        if [ "$UNKNOWN_LANGS_FOUND" = false ]; then
            pass "All code fence language tags are recognized (or handled by hook)"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════
# Phase 3: MkDocs build validation
# ══════════════════════════════════════════════════════════════════
section "Phase 3: MkDocs build validation (mkdocs build --strict)"

SITE_DIR="$REPO_ROOT/site"
BUILD_LOG=$(mktemp)
trap 'rm -f "$BUILD_LOG"' EXIT

if "$MKDOCS" build --strict 2>"$BUILD_LOG"; then
    pass "mkdocs build --strict completed successfully"
else
    fail "mkdocs build --strict failed"
    # Show first 20 lines of errors
    echo ""
    echo -e "  ${RED}Build output:${NC}"
    head -20 "$BUILD_LOG" | while IFS= read -r line; do
        echo "    $line"
    done
    echo ""
fi

# Check build log for warnings even if build succeeded
if [ -s "$BUILD_LOG" ]; then
    WARNING_COUNT=$(grep -ciE '(warning|warn)' "$BUILD_LOG" || true)
    if [ "$WARNING_COUNT" -gt 0 ]; then
        warn "Build produced $WARNING_COUNT warning(s)"
        grep -iE '(warning|warn)' "$BUILD_LOG" | head -10 | while IFS= read -r line; do
            echo "    $line"
        done
    fi
fi
# ══════════════════════════════════════════════════════════════════
# Phase 4: Post-build HTML validation
# ══════════════════════════════════════════════════════════════════
section "Phase 4: Post-build HTML validation (site/)"

if [ ! -d "$SITE_DIR" ]; then
    fail "site/ directory does not exist — cannot perform post-build validation"
    info "Run 'mkdocs build' first"
else
    # Collect all HTML files
    mapfile -t HTML_FILES < <(find "$SITE_DIR" -name '*.html' -type f | sort)

    if [ ${#HTML_FILES[@]} -eq 0 ]; then
        fail "No HTML files found in site/"
    else
        info "Scanning ${#HTML_FILES[@]} HTML file(s) in site/"

        # 4a. Check for leftover rust,ignore / rust,no_run / rust,compile_fail
        #     in rendered output (the hook should have transformed these)
        RUSTDOC_LEFTOVERS=false
        RUSTDOC_CLASS_PATTERN='class="[^"]*language-rust,(ignore|no_run|compile_fail|edition[0-9]+)[^"]*"'
        for html_file in "${HTML_FILES[@]}"; do
            rel_path="${html_file#"$REPO_ROOT"/}"
            # Only flag rustdoc annotations that appear inside class="language-..."
            # attributes (i.e., unprocessed code blocks). Prose mentions in <p>,
            # <li>, or <code> tags are intentional and should not be flagged.
            matches=$(grep -cE "$RUSTDOC_CLASS_PATTERN" "$html_file" || true)
            if [ "$matches" -gt 0 ]; then
                fail "Found $matches leftover rustdoc annotation(s) in $rel_path"
                grep -nE "$RUSTDOC_CLASS_PATTERN" "$html_file" | head -5 | while IFS= read -r line; do
                    echo "      $line"
                done
                RUSTDOC_LEFTOVERS=true
            fi
        done
        if [ "$RUSTDOC_LEFTOVERS" = false ]; then
            pass "No leftover rustdoc annotations (rust,ignore etc.) in rendered HTML"
        fi

        # 4b. Check for mermaid diagrams rendered as plain text
        #     Mermaid keywords outside <pre class="mermaid"> indicate broken rendering.
        #     We look for mermaid diagram start keywords appearing inside generic
        #     code blocks or <p> tags instead of mermaid containers.
        MERMAID_PLAINTEXT=false
        MERMAID_KEYWORDS='(graph[[:space:]]+(TD|TB|BT|RL|LR)|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|pie[[:space:]]+title|flowchart[[:space:]]+(TD|TB|BT|RL|LR)|gitGraph)'
        for html_file in "${HTML_FILES[@]}"; do
            rel_path="${html_file#"$REPO_ROOT"/}"
            # Find mermaid keywords that appear in the file
            keyword_matches=$(grep -cE "$MERMAID_KEYWORDS" "$html_file" || true)
            if [ "$keyword_matches" -gt 0 ]; then
                # Check if keywords appear inside language-text or <p> tags
                # which would indicate they were NOT rendered as diagrams.
                # Exclude matches inside <code> inline elements within <p>
                # tags, since those are prose explanations of mermaid syntax.
                really_bad=$(grep -E "$MERMAID_KEYWORDS" "$html_file" \
                    | grep -E '(class="language-text"|<p>)' \
                    | grep -vE '<code>[^<]*'"$MERMAID_KEYWORDS" || true)
                if [ -n "$really_bad" ]; then
                    fail "Mermaid diagram keyword rendered as plain text in $rel_path"
                    echo "$really_bad" | head -3 | while IFS= read -r line; do
                        trimmed="${line:0:200}"
                        echo "      $trimmed"
                    done
                    MERMAID_PLAINTEXT=true
                fi
            fi
        done
        if [ "$MERMAID_PLAINTEXT" = false ]; then
            pass "No mermaid diagrams rendered as plain text"
        fi

        # 4c. Check for Rust code misclassified as language-text
        #     If a code block has class="language-text" but contains Rust keywords,
        #     the hook likely failed to transform the language tag.
        #     We extract only the content within each language-text block (up to
        #     the closing </pre>) and require at least 2 strong Rust indicators
        #     to avoid false positives from error messages or CLI output that
        #     happens to mention Rust paths.
        MISCLASSIFIED=false
        # Strong Rust indicators: fn/struct/impl/enum/trait declarations,
        # derive macros, async_trait, use-with-semicolon, let mut bindings.
        # These are things that appear in actual Rust source code, not in
        # compiler error messages or CLI output.
        RUST_STRONG='(pub[[:space:]]+(fn|struct|enum|trait|mod)[[:space:]]|#\[derive|#\[async_trait|async[[:space:]]+fn[[:space:]]+[[:alnum:]_]+|impl[[:space:]]+[[:alnum:]_]+[[:space:]]+for[[:space:]]|impl<|fn[[:space:]]+[[:alnum:]_]+[[:space:]]*\(|let[[:space:]]+mut[[:space:]]+[[:alnum:]_]+[[:space:]]*=|tokio::(main|spawn|select))'
        for html_file in "${HTML_FILES[@]}"; do
            rel_path="${html_file#"$REPO_ROOT"/}"
            if grep -q 'class="language-text' "$html_file"; then
                # Extract content within each language-text block
                # (from class="language-text" up to the next </pre>)
                block_content=$(sed -n '/class="language-text/,/<\/pre>/p' "$html_file")
                rust_hits=$(echo "$block_content" | grep -cE "$RUST_STRONG" || true)
                if [ "$rust_hits" -ge 2 ]; then
                    fail "Code block classified as language-text appears to contain Rust code in $rel_path ($rust_hits Rust indicators found)"
                    info "This usually means the rustdoc_codeblocks hook failed to transform a fence tag"
                    MISCLASSIFIED=true
                fi
            fi
        done
        if [ "$MISCLASSIFIED" = false ]; then
            pass "No Rust code blocks misclassified as language-text"
        fi

        # 4d. Check for broken code fences showing as raw markdown
        #     If triple backticks appear inside <p> tags in the rendered HTML,
        #     a code fence was not parsed correctly.
        BROKEN_FENCES=false
        for html_file in "${HTML_FILES[@]}"; do
            rel_path="${html_file#"$REPO_ROOT"/}"
            matches=$(grep -cE '<p>[[:space:]]*```' "$html_file" || true)
            if [ "$matches" -gt 0 ]; then
                fail "Broken code fence(s) in $rel_path — triple backticks rendered as <p> content"
                grep -nE '<p>[[:space:]]*```' "$html_file" | head -5 | while IFS= read -r line; do
                    echo "      $line"
                done
                BROKEN_FENCES=true
            fi
        done
        if [ "$BROKEN_FENCES" = false ]; then
            pass "No broken code fences (raw backticks in <p> tags)"
        fi

        # 4e. Check for HTML error indicators
        #     Some MkDocs themes inject error messages for build problems.
        HTML_ERRORS=false
        for html_file in "${HTML_FILES[@]}"; do
            rel_path="${html_file#"$REPO_ROOT"/}"
            if grep -qiE 'class="(admonition[[:space:]]+)?error".*(could not|failed to|unable to)' "$html_file"; then
                warn "Possible rendering error message detected in $rel_path"
                HTML_ERRORS=true
            fi
        done
        if [ "$HTML_ERRORS" = false ]; then
            pass "No HTML error indicators in rendered pages"
        fi
    fi
fi

# ══════════════════════════════════════════════════════════════════
# Summary
# ══════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}── Summary ──${NC}"
echo ""
echo -e "  Total checks:  $TOTAL_CHECKS"
echo -e "  ${GREEN}Passed:${NC}        $CHECKS_PASSED"
if [ "$WARNINGS" -gt 0 ]; then
    echo -e "  ${YELLOW}Warnings:${NC}      $WARNINGS"
fi
if [ "$FAILURES" -gt 0 ]; then
    echo -e "  ${RED}Failures:${NC}      $FAILURES"
fi
echo ""

if [ "$FAILURES" -gt 0 ]; then
    echo -e "${RED}FAILED: $FAILURES check(s) failed. Fix issues before deploying docs.${NC}"
    exit 1
else
    if [ "$WARNINGS" -gt 0 ]; then
        echo -e "${GREEN}PASSED${NC} (with $WARNINGS warning(s)). Documentation should render correctly."
    else
        echo -e "${GREEN}PASSED: All documentation rendering checks passed.${NC}"
    fi
    exit 0
fi
