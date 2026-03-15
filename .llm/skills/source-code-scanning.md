# Source Code Scanning in Tests

Reference for writing tests and scripts that scan Rust source files for patterns — handling string literals, raw strings, comments, and directory traversal correctly.

## Raw String Handling

### The problem

Rust raw strings (`r#"..."#`, `r##"..."##`, etc.) contain embedded quotes that naive parsers mishandle. A scanner toggling `in_string` on every `"` will exit string mode early on `r#"{"type":"ping"}"#`, leaking raw-string contents into scanned code tokens.

This is a real and common problem in this codebase: `src/polling_client.rs` and `tests/protocol_tests.rs` contain many raw-string JSON literals with embedded quotes.

### The fix: `strip_non_code()`

The canonical implementation lives in `tests/ci_config_tests.rs` (top-level scope). It handles:

- Regular strings with backslash escapes (`"hello \"world\""`)
- Raw strings with any hash count (`r"..."`, `r#"..."#`, `r##"..."##`)
- Raw identifiers (`r#type`) — NOT treated as raw strings
- Line comments (`//`)
- Inline block comments (`/* ... */`) on a single line
- Multi-line block comments via `strip_non_code_stateful()` variant
- URLs inside strings (not confused with `//` comments)
- `/* */` delimiters inside strings (not confused with block comments)

**Always use `strip_non_code_stateful()` when iterating over file lines.** The single-line `strip_non_code()` wrapper is only for unit tests and one-off single-line checks. Do not use bare `line.contains(pattern)` on raw source — it will match inside string literals and comments.

### Callers

| Function | Variant Used | Purpose |
|----------|-------------|---------|
| `is_crate_referenced_in_dir()` | `strip_non_code_stateful` | Dev-dependency usage scanner |
| `test_files_avoid_bare_unwrap_on_io_operations()` | `strip_non_code_stateful` | I/O unwrap policy |
| `line_references_crate()` | (operates on stripped output) | Word-boundary crate name matching |
| Single-line unit tests | `strip_non_code` | Testing the stripping logic itself |

### Raw string delimiter algorithm

1. When not in a string: if byte is `r`, check that the preceding byte is NOT alphanumeric/underscore (word boundary).
2. Count consecutive `#` bytes after `r`.
3. If the next byte after the `#`s is `"`, enter raw-string mode with that hash count.
4. In raw-string mode: skip all content until `"` followed by exactly the same number of `#` bytes.
5. Raw strings with zero hashes (`r"..."`) work identically to regular strings but without escape processing.

## Directory Traversal

### Always walk recursively when the test name implies full coverage

If a test is named `all_docs_*` or claims to check "every file" in a directory, it **must** use recursive traversal. `std::fs::read_dir` only reads one level.

**Pattern to use** (matches existing `scan_dir` and `collect_md_files` helpers):

```rust,ignore
fn collect_md_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!("Failed to read '{}': {e}", dir.display());
    }) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}
```

### When flat traversal is intentional

If a test intentionally only checks top-level files, add a comment explaining the scope. Example: checking only root-level TOML files, or only top-level workflow YAML files.

## Documentation Snippet Validation

### Check all syntactic forms

Dependency version snippets in docs can appear in multiple TOML forms:

| Form | Example | Notes |
|------|---------|-------|
| Inline table | `signal-fish-client = { version = "X.Y.Z" }` | Most common |
| Bare string | `signal-fish-client = "X.Y.Z"` | Simple form |
| With features | `signal-fish-client = { version = "X.Y.Z", features = [...] }` | Extended |

Tests validating version consistency must handle **all** forms, not just inline tables. The canonical implementation in `all_docs_dependency_snippets_use_cargo_package_version` handles both the `version = "..."` keyword form and the bare `= "..."` string form, including trailing TOML comments.

### Whitespace-tolerant TOML value matching

Never use exact substring matching (e.g., `contains("version = \"0.4.1\"")`) to check TOML key-value pairs. TOML allows arbitrary whitespace around `=`, so `version="0.4.1"`, `version = "0.4.1"`, and `version  =  "0.4.1"` are all valid.

Use the `text_contains_version_value(text, version)` helper in `ci_config_tests.rs` instead. It finds `version`, skips whitespace, matches `=`, skips whitespace, then checks the quoted value. This avoids false negatives from formatting differences.

**General rule:** When matching structured data (TOML, YAML, JSON) in tests or scripts, parse the value rather than matching a literal formatted string. For TOML: use the `toml` crate or a whitespace-tolerant manual parser. For YAML/JSON: use the appropriate parser crate.

## Exception Constant Naming

When creating exception lists for scanner tests (e.g., dependencies the scanner cannot detect), name the constant after **what the scanner cannot handle**, not after a single specific case:

- **Good:** `DEV_DEP_USAGE_EXCEPTIONS` — covers any reason a dev-dep might be undetectable
- **Bad:** `INDIRECT_USE_EXCEPTIONS` — implies all entries are "indirect" when some may be dual-listed or scanner-limited

Each entry must include a reason string explaining **why** the exception exists. Helper tests (`*_exceptions_are_documented`, `*_exceptions_are_actual_dev_dependencies`) enforce this. When adding a new exception, describe the specific scanner limitation that requires it.

## Block Comment Handling

`strip_non_code()` handles inline block comments (`/* ... */`) on a single line.
For multi-line block comments that span across lines, use
`strip_non_code_stateful(line, &mut in_block_comment)` which tracks whether
the scanner is inside an open block comment across successive calls.

All callers that iterate over file lines must use `strip_non_code_stateful`:

```rust,ignore
let mut in_block_comment = false;
for line in contents.lines() {
    let code_only = strip_non_code_stateful(line, &mut in_block_comment);
    // ... match on code_only ...
}
```

Do **not** use `strip_non_code()` in a per-line loop — it cannot detect
multi-line `/* ... */` comments and will produce false positives on lines
that are entirely inside a block comment.

## Silent-Pass Anti-Pattern

When a test conditionally parses a value (e.g., via `Option` or `if let`),
it must **fail explicitly** when parsing returns `None`/`Err` on input that
was expected to be parseable. Otherwise the test silently passes on
malformed input and cannot catch regressions.

Bad (silent pass):

```rust,ignore
if let Some(version) = extract_version(line) {
    assert_eq!(version, expected);
}
// If extract_version returns None, no assertion fires — test passes silently
```

Good (explicit failure):

```rust,ignore
let version = extract_version(line).unwrap_or_else(|| {
    panic!("Could not parse version from line: `{line}`");
});
assert_eq!(version, expected);
```

Apply this rule to any test that detects a pattern and then conditionally
asserts on a parsed value. If the pattern was detected, parsing must succeed.

## Checklist for New Source Scanners

When writing a new test or script that scans `.rs` files for patterns:

1. Use `strip_non_code_stateful()` (not `strip_non_code()`) when iterating
   over file lines to correctly handle multi-line block comments
2. Use `line_references_crate()` for word-boundary-aware identifier matching
3. Use recursive traversal if the test claims to cover "all" files in a directory
4. Handle all relevant syntactic forms (not just the most common one)
5. Add tests for raw strings with inner quotes, raw identifiers, and nested directories
6. Never silently skip unparseable input — fail with a descriptive message
