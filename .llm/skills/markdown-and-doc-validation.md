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

## YAML Workflow Snippet Shape Validation

Fenced YAML workflow examples should keep step keys aligned under the same
list-item mapping. In a step like `- name: ...`, sibling keys (`uses`, `with`,
`run`) must align with `name` (not be over-indented).

The pre-commit hook validates this shape in `.llm/*.md` fenced YAML blocks and
fails on malformed snippets, preventing docs examples from drifting into invalid
workflow structure.

## Numbered Workflow Comment Drift

### The Bug Pattern

When `main()` workflow comments use numbered labels (`# 1.`, `# 2.`, ...),
adding a new step can leave duplicate or skipped numbers. This is easy to miss
in review and makes maintenance comments misleading.

### The Fix

Keep numbered workflow comments contiguous and update all subsequent numbers
after inserting a new step.

### Validation Pattern

Add a regression test that parses the `main()` function source and asserts:

- Numbered workflow comments exist.
- The sequence starts at `1`.
- Numbers are contiguous with no duplicates/gaps.

In this repo, `scripts/test_pre_commit_llm.py` enforces this for
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

## Changelog Reference Link Consistency

Keep a Changelog examples must keep version links synchronized.

Scope reminder: changelog entries are for user-visible behavior/API changes only.
Do not add internal-only implementation items (CI/scripts/tests/refactors with no
consumer impact).

- `[Unreleased]` should compare from the latest released tag: `.../compare/vX.Y.Z...HEAD`
- Latest version link (`[X.Y.Z]`) should either:
  - point to `.../releases/tag/vX.Y.Z`, or
  - compare previous-to-latest: `.../compare/vPREV...vX.Y.Z`

The pre-commit hook enforces this for `.llm/*.md` files.

### Error Handling in Validation Functions

Every validation function that reads files or checks the filesystem must
handle `OSError` (Python) or propagate errors cleanly (Rust). Unhandled
I/O errors crash the entire validation pipeline rather than producing a
clear, actionable failure message.

*Pattern (Python):*

```python
def validate_something() -> list[str]:
    errors = []
    try:
        content = path.read_text(encoding="utf-8")
    except OSError as e:
        errors.append(f"  Could not read {path}: {e}")
        return errors
    # ... parse content ...
    for item in items:
        try:
            exists = item_path.is_file()
        except OSError as e:
            errors.append(f"  Could not check {item_path}: {e}")
            continue
    return errors
```

*Checklist for validation functions:*

- [ ] Is every `read_text()` / `read_to_string()` wrapped in error
      handling?
- [ ] Are filesystem checks (`is_file()`, `is_dir()`) wrapped in error
      handling for non-critical paths?
- [ ] Do error messages follow the same format as existing validators
      (indented with two spaces, descriptive)?
- [ ] Does the function continue checking remaining items after a
      single I/O error (rather than aborting entirely)?

### Comment Accuracy in Comparison Logic

When validation code builds a collection for comparison (e.g., a
`HashSet` of nav references), the comment must accurately describe
what the collection contains and how comparisons work. Misleading
comments about "basenames" vs "full paths" can hide subtle bugs.

*Checklist for comparison logic comments:*

- [ ] Does the comment accurately describe what is collected (full
      paths, basenames, normalized forms)?
- [ ] Does the comment explain the comparison semantics (exact match,
      contains, prefix)?
- [ ] If the comparison scope is limited (e.g., top-level files only),
      is that limitation clearly stated?

## Navigation Card Label Consistency

### The Bug Pattern

Navigation cards in `docs/index.md` use the pattern
`[:octicons-arrow-right-24: LABEL](FILENAME)`. When a page's H1 heading
is updated but the card label is not, the two drift apart — confusing
users who see one title on the landing page and a different title on the
actual page.

### The Fix

Always use the target page's H1 heading as the card label. When
renaming a page, update both the page heading **and** the card in
`docs/index.md`.

### Validation Layers

1. **Rust test** (`tests/ci_config_tests.rs` `docs_nav_card_consistency`):
   Extracts card links from `docs/index.md`, reads each target file's
   H1, and asserts they match. Runs on every `cargo test`.

2. **Pre-commit hook** (`scripts/pre-commit-llm.py`
   `validate_doc_nav_card_consistency`): Same check at commit time.
   Blocks commits with mismatched labels.

### Checklist for Adding or Editing Nav Cards

- [ ] Does the card label exactly match the H1 of the target page?
- [ ] If the target page's H1 changed, is the card label updated too?
- [ ] Does `cargo test` pass the `nav_card_labels_match_page_titles`
      test?

## Documentation Drift Validation

Detailed guidance for cross-referencing docs against source-of-truth config
files was split into `doc-drift-validation.md` to keep this skill under the
pre-commit line limit.

Use that guide for:

- `scripts/validate-devcontainer-docs.sh` style checks
- Checklist for when to add new doc-vs-config validators
- JSONC-safe matching patterns and failure-message conventions
