# Markdown Parsing and Documentation Validation

Reference for correctly parsing markdown content in scripts and preventing documentation drift against source-of-truth config files.

## Markdown Parsing: Fenced Code Blocks

### The Bug Pattern

Any script or tool that reads markdown line-by-line and extracts content
(headings, paragraphs, links, Rust snippets) **must** track fenced code
block state. Without this, lines inside a code fence are misinterpreted
as real content.

Example: a function extracting "the first paragraph" from a skill file
would return `[dependencies]` from a TOML code block instead of the
actual description paragraph below it.

### The Fix: Track Fence Character Type

Maintain a `fence_char` variable (not a simple boolean) so that
backtick fences (`` ``` ``) and tilde fences (`~~~`) are matched
correctly per CommonMark:

```python
fence_char = None          # None = outside any fence
for line in text.splitlines():
    stripped = line.strip()
    if stripped.startswith("```") or stripped.startswith("~~~"):
        char = stripped[0]  # '`' or '~'
        if fence_char is None:
            fence_char = char          # open fence
        elif char == fence_char:
            fence_char = None          # close only with same char
        continue
    if fence_char is not None:
        continue  # skip all content inside fences
    # ... process the line normally
```

**Key rules:**

1. Recognize **both** `` ``` `` and `~~~` as fence openers (with or
   without a language tag such as `` ```rust `` or `~~~toml`).
   Per CommonMark, backtick and tilde fences track independently --
   a tilde fence cannot be closed by backticks and vice versa.
2. While inside a fence, **skip everything** -- headings, blank lines,
   and apparent paragraph text are all fence content.
3. A fence is closed only by a line starting with the **same** character
   type that opened it; processing resumes on the next line.
4. **Known simplification:** the parser does not track fence length
   (CommonMark requires the closing fence to have >= the opening count
   of backticks/tildes). This is acceptable for `.llm/skills/` files.

### Where This Applies in the Codebase

| Script | What it parses | How it handles fences |
|--------|---------------|-----------------------|
| `scripts/pre-commit-llm.py` | Skill file paragraphs and titles for index generation | `extract_first_paragraph` and `extract_title` track `fence_char` |
| `scripts/extract-rust-snippets.sh` | Markdown files for ```` ```rust ```` blocks | Tracks `in_rust_block` flag, extracts only Rust fences |

### Testing the Parser

`scripts/test_pre_commit_llm.py` contains pytest regression tests for
the code-fence parsing fix. Key test cases:

- `test_code_fence_contents_never_leak_into_result` -- the original
  regression test ensuring fence content is never returned as paragraph
  text.
- `test_code_fence_with_language_specifier` -- verifies language-tagged
  fences (`` ```rust ``, `` ```toml ``) are recognized.
- `test_heading_inside_code_fence_is_ignored` -- headings inside a fence
  are not treated as real headings.
- `test_paragraph_immediately_followed_by_code_fence` -- paragraph ends
  cleanly when a fence opens without a blank line separator.

Run the tests:

```shell
pytest scripts/test_pre_commit_llm.py -v
```

### Checklist for New Markdown-Parsing Code

- [ ] Does the parser track fenced code block state?
- [ ] Does it handle both `` ``` `` and `~~~` fences, closing only
      when the same fence character type is encountered?
- [ ] Are all lines inside a fence unconditionally skipped?
- [ ] Is there a test that places misleading content inside a fence and
      asserts it never appears in the output?

## MkDocs Nav Validation

### The Bug Pattern

The `mkdocs.yml` `nav:` section references markdown files that must exist
in the `docs/` directory. If a nav entry references a file that does not
exist in `docs/`, `mkdocs build --strict` fails in CI.

### The Fix: Keep Root-Level Files Out of MkDocs Nav

Files that live at the project root (e.g., `CHANGELOG.md`) should **not**
be referenced in the MkDocs `nav:` section because MkDocs requires all
nav entries to resolve to files inside the `docs/` directory. Instead,
keep root-level files as standalone project documents and link to them
from docs pages where appropriate (e.g., a "see CHANGELOG.md" note),
rather than creating thin wrapper files in `docs/` with snippet includes.

### The Validation Pattern

Three layers of validation catch this bug:

1. **Rust test** (`tests/ci_config_tests.rs` `mkdocs_nav_validation`):
   Parses `mkdocs.yml` nav entries, verifies each `.md` reference exists
   in `docs/`. Runs on every `cargo test` invocation, including CI.

2. **Pre-commit hook** (`scripts/pre-commit-llm.py` `validate_mkdocs_nav`):
   Same check at commit time. Blocks commits that would break MkDocs.

3. **CI validate script** (`scripts/ci-validate.sh` check 9):
   Shell-based nav validation for the local CI validation suite.

### Checklist for New Nav Entries

- [ ] Does the referenced `.md` file exist in `docs/`?
- [ ] If the source file lives at the project root, avoid adding it to
      the nav -- link to it from a docs page instead.
- [ ] Does `mkdocs build --strict` pass locally?

## Documentation Validation: Preventing Drift

### The Problem

README files and other docs can claim that a config file contains a
certain key, setting, or hook -- but the config may have changed since
the docs were last updated. This is **documentation drift**.

### The Fix: Cross-Reference Docs Against Config

The `scripts/validate-devcontainer-docs.sh` script demonstrates the
pattern:

1. Define the set of keys that matter (e.g., devcontainer lifecycle
   hooks).
2. For each key mentioned in the documentation, verify it exists as an
   actual key in the source-of-truth config file.
3. Report mismatches as errors.

```bash
for hook in "${LIFECYCLE_HOOKS[@]}"; do
    if grep -qw "$hook" "$README"; then
        if ! grep -qE "^[[:space:]]*\"${hook}\"[[:space:]]*:" "$CONFIG"; then
            echo "MISMATCH: '$hook' documented but missing from config"
            errors=$((errors + 1))
        fi
    fi
done
```

### Existing Validation in the Codebase

| Mechanism | What it catches |
|-----------|----------------|
| `scripts/validate-devcontainer-docs.sh` | Devcontainer README referencing hooks not present in `devcontainer.json` |
| `tests/ci_config_tests.rs` `msrv_consistent_across_key_files` | MSRV value drifting between `Cargo.toml` and docs |
| `tests/ci_config_tests.rs` `config_existence` | Config files referenced by CI but missing from repo |

### Checklist for New Documentation

When adding docs that reference configuration:

- [ ] Is there a validation script or test that cross-references the
      doc against the config source of truth?
- [ ] Does the CI pipeline run that validation?
- [ ] If the config format is JSONC (comments allowed), does the
      validation use `grep` rather than a strict JSON parser?

### When to Add a New Validation Script

Add a validation script when:

- A new config file is introduced alongside documentation that
  describes its contents.
- A README enumerates settings, keys, or hooks from a config file.
- Multiple files must stay in sync (e.g., MSRV across `Cargo.toml`,
  CI workflows, and docs).

Follow the pattern in `validate-devcontainer-docs.sh`:

1. Resolve `REPO_ROOT` from `$SCRIPT_DIR/..`.
2. Use `grep` for JSONC-safe matching (no JSON parser needed).
3. Exit with code 1 on any mismatch.
4. Print a clear message naming both the doc and the config file.
