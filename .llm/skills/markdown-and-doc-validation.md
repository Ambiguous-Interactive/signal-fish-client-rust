# Markdown Parsing and Documentation Validation

Reference for correctly parsing markdown content in scripts, preventing documentation drift, and avoiding MkDocs rendering issues.

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

### Using `rust,ignore` for Non-Compilable Snippets

Markdown code blocks that reference external crates not in the snippet
project's dependencies, or that use feature-gated types unavailable during
extraction, should use ```` ```rust,ignore ```` instead of ```` ```rust ````.
The `extract-rust-snippets.sh` script recognizes `rust,ignore` and skips
those blocks during compilation checks.

**Decision rule:** only use plain `` ```rust `` for complete,
self-contained, compilable programs. Everything else gets
`` ```rust,ignore ``. When in doubt, use `rust,ignore`. Use `rust,ignore`
when a snippet depends on external crates, uses platform-specific or
feature-gated APIs, is pseudo-code/illustrative, contains bare signatures
without a body, or imports platform-specific modules.

### Testing the Parser

`scripts/test_pre_commit_llm.py` contains pytest regression tests for
the code-fence parsing fix. Key tests: fence contents never leak into
results, language-tagged fences are recognized, headings inside fences
are ignored, and paragraphs followed by fences end cleanly.

```shell
pytest scripts/test_pre_commit_llm.py -v
```

### Checklist for New Markdown-Parsing Code

- [ ] Does the parser track fenced code block state?
- [ ] Does it handle both `` ``` `` and `~~~` fences, closing only
      when the same fence character type is encountered?
- [ ] Are all lines inside a fence unconditionally skipped?
- [ ] Is there a test with misleading content inside a fence?

## MkDocs Rendering: Code Fence Languages and Mermaid

### Rustdoc Code-Fence Annotations

Rustdoc annotations like `rust,ignore`, `rust,no_run`, and
`rust,compile_fail` are valid in source markdown for the snippet
extraction pipeline (see above). However, Pygments (used by
`pymdownx.highlight`) does not recognize these compound language
tags and falls back to plain-text rendering, which can also corrupt
everything after the block.

**The build-time hook** at `hooks/rustdoc_codeblocks.py` fixes this
automatically. It strips the `,<annotation>` suffix so Pygments
receives plain `rust`. Source files are **not** modified; the
transformation is applied only during `mkdocs build`.

The hook is registered in `mkdocs.yml` under `hooks:`.

**Authors do not need to avoid `rust,ignore`** -- the hook handles it.
But understanding the system prevents confusion when rendered output
shows plain `rust` highlighting for a `rust,ignore` block.

### Code Fence Language Compatibility

| Language tag | Pygments | Rustdoc | Notes |
|-------------|----------|---------|-------|
| `rust` | OK | OK | Use for compilable snippets |
| `rust,ignore` | Broken (fixed by hook) | OK | Non-compilable snippets |
| `rust,no_run` | Broken (fixed by hook) | OK | Compiles but not executed |
| `rust,compile_fail` | Broken (fixed by hook) | OK | Expected compilation failure |
| `rust,edition20XX` | Broken (fixed by hook) | OK | Edition-specific snippet |
| `python`, `shell`, `toml`, `json`, `yaml`, `text` | OK | N/A | Standard Pygments lexers |
| `mermaid` | N/A | N/A | Handled by `custom_fences` (see below) |

### Mermaid Diagram Best Practices

Mermaid diagrams render via `pymdownx.superfences` `custom_fences`
configuration in `mkdocs.yml`. The required config:

```yaml
- pymdownx.superfences:
    custom_fences:
      - name: mermaid
        class: mermaid
        format: !!python/name:pymdownx.superfences.fence_code_format
```

**Authoring guidelines:**

- Use ```` ```mermaid ```` as the fence language (lowercase, no quotes).
- Supported diagram types: `graph`, `stateDiagram-v2`, `sequenceDiagram`,
  `flowchart`, `classDiagram`, `erDiagram`, `gantt`, `pie`, `gitgraph`.
- Keep diagrams simple -- complex diagrams with many nodes render poorly
  on mobile. Prefer `graph LR` (left-to-right) for linear flows.
- Quote node labels containing special characters:
  `A["Transport (trait)"]` not `A[Transport (trait)]`.
- Do **not** add a blank line between the opening fence and the first
  diagram directive (`graph LR`, `sequenceDiagram`, etc.).
- Test rendering locally with `mkdocs serve` before committing.

### Testing Docs Rendering Locally

```shell
# Full strict build (matches CI)
mkdocs build --strict

# Live preview with hot-reload
mkdocs serve
```

CI runs `mkdocs build --strict` in `.github/workflows/docs-deploy.yml`.
Any Pygments warning or broken reference fails the build. The deploy
workflow also performs a post-build verification that greps rendered HTML
for unresolved rustdoc annotation class names (e.g., `language-rust,ignore`)
and fails the build if any survive the hook. Always run `mkdocs build
--strict` locally before pushing docs changes.

## YAML Workflow Snippet Shape Validation

Fenced YAML workflow examples should keep step keys aligned under the same
list-item mapping. In a step like `- name: ...`, sibling keys (`uses`, `with`,
`run`) must align with `name` (not be over-indented).

The pre-commit hook validates this shape in `.llm/*.md` fenced YAML blocks and
fails on malformed snippets, preventing docs examples from drifting into invalid
workflow structure.

## Numbered Workflow Comment Drift

When `main()` workflow comments use numbered labels (`# 1.`, `# 2.`, ...),
adding a new step can leave duplicate or skipped numbers. Keep numbered
comments contiguous and update all subsequent numbers after inserting a
new step. `scripts/test_pre_commit_llm.py` enforces this for
`scripts/pre-commit-llm.py`.

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

### Comment Filtering in mkdocs.yml Nav Extraction

When extracting `.md` filenames from `mkdocs.yml`, filter out YAML comments (`grep -vE '^[[:space:]]*#'`) **before** running extraction (`grep -oE`). Extraction discards the leading `#`, making a downstream comment filter a no-op. See `ci-configuration.md` "Pipeline ordering for context-dependent filters" for the general rule. Reference: `scripts/validate-docs.sh` uses the correct ordering.

## Changelog Reference Link Consistency

Keep a Changelog examples must keep version links synchronized.
Scope reminder: changelog entries are for user-visible behavior/API changes
only -- not internal CI/scripts/tests/refactors with no consumer impact.

- `[Unreleased]` should compare from the latest released tag: `.../compare/vX.Y.Z...HEAD`
- Latest version link (`[X.Y.Z]`) should point to `.../releases/tag/vX.Y.Z`
  or compare previous-to-latest: `.../compare/vPREV...vX.Y.Z`

The pre-commit hook enforces this for `.llm/*.md` files.

### Error Handling in Validation Functions

Every validation function must handle `OSError` (Python) or propagate
errors cleanly (Rust). Wrap `read_text()`, `is_file()`, and `is_dir()`
calls. Continue after a single I/O error (do not abort entirely). Use
two-space-indent, descriptive error messages matching the existing format.

### Comment Accuracy in Comparison Logic

When validation code builds a collection for comparison (e.g., a
`HashSet` of nav references), ensure comments accurately describe what is
collected (full paths, basenames, normalized forms), the comparison
semantics (exact match, contains, prefix), and any scope limitations.

## Navigation Card Label Consistency

Navigation cards in `docs/index.md` use the pattern
`[:octicons-arrow-right-24: LABEL](FILENAME)`. Always use the target
page's H1 heading as the card label. When renaming a page, update both
the page heading **and** the card in `docs/index.md`.

Validated by `tests/ci_config_tests.rs` `docs_nav_card_consistency` and
`scripts/pre-commit-llm.py` `validate_doc_nav_card_consistency`.

## Lychee Link Checker: Header Format

In `.lychee.toml`, headers use `key=value` (equals), **not** `key: value`
(colon). lychee v0.18+ rejects colon syntax with `Header value must be of the form key=value`.

### Shell Script Portability in Documentation Scripts

Scripts that validate documentation (e.g., `check-docs-rendering.sh`, `validate-docs.sh`) must use portable regex. See the "Portable regex" section in `ci-configuration.md` for the full rules. The `test_shell_portability.sh` script and `ci_config_tests.rs` Rust tests enforce this.

## Common Markdownlint Pitfalls

- **MD032** (blank lines around lists): Add a blank line before and after every
  list block, including after introductory paragraphs ending with a colon.
- **MD040** (code fence language): Always add a language specifier to fenced code
  blocks (e.g., ` ```rust `, ` ```toml `, ` ```text `).
- **MD026** (trailing punctuation in headings): Do not end headings with `.`,
  `:`, `;`, or `!`.

## Documentation Drift Validation

Detailed guidance for cross-referencing docs against source-of-truth config
files was split into `doc-drift-validation.md` to keep this skill under the
pre-commit line limit.

Use that guide for:

- `scripts/validate-devcontainer-docs.sh` style checks
- Checklist for when to add new doc-vs-config validators
- JSONC-safe matching patterns and failure-message conventions
