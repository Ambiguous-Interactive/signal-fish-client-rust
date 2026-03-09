# Shell Script Portability

Reference for writing portable shell scripts that work on both GNU (Linux) and BSD (macOS) systems.

## Output: Use `printf` Instead of `echo`

`echo` behavior varies across shells and platforms. Always use `printf` for reliable output.

### `echo "$var" | cmd` Is Unsafe

`echo` may interpret backslash sequences or mangle variables starting with `-n`, `-e`, or `-E`. Use `printf '%s\n'` instead:

```shell
# WRONG — may mangle content (leading -n, backslash interpretation)
echo "$var" | sed 's/foo/bar/'

# CORRECT — printf is POSIX-defined and portable
printf '%s\n' "$var" | sed 's/foo/bar/'
```

### `echo -n` and `echo -e` Are Non-Portable

Neither `-n` (suppress newline) nor `-e` (enable backslash escapes) are defined by POSIX. Use `printf` equivalents:

```shell
# WRONG — -n is non-portable
echo -n "no newline"

# CORRECT
printf '%s' "no newline"

# WRONG — -e is non-portable
echo -e "line1\nline2"

# CORRECT
printf 'line1\nline2\n'
```

## Regex: POSIX Character Classes

PCRE shorthand character classes are not recognized by POSIX `grep -E` or `sed -E`. Use POSIX bracket expressions instead.

### Character Class Translation Table

| PCRE | POSIX (ERE) | Meaning |
|------|-------------|---------|
| `\s` | `[[:space:]]` | Whitespace |
| `\S` | `[^[:space:]]` | Non-whitespace |
| `\d` | `[[:digit:]]` or `[0-9]` | Digit |
| `\D` | `[^[:digit:]]` | Non-digit |
| `\w` | `[[:alnum:]_]` | Word character |
| `\W` | `[^[:alnum:]_]` | Non-word character |
| `\b` | N/A (see below) | Word boundary |
| `(?:...)` | `(...)` | Non-capturing group (ERE has no distinction) |

### Word Boundaries (`\b`) Are GNU Extensions

`\b` is not available in POSIX ERE. Use `grep -w` for whole-word matching or explicit boundary patterns:

```shell
# WRONG — \b is a GNU extension
grep -E '\bword\b' file.txt

# CORRECT — grep -w matches whole words
grep -w 'word' file.txt

# CORRECT — explicit boundary pattern
grep -E '([^[:alnum:]_]|^)word([^[:alnum:]_]|$)' file.txt
```

## Tool Flags: GNU vs POSIX

### `sed -r` Is GNU-Only

GNU `sed` uses `-r` for extended regex. POSIX and BSD `sed` use `-E`. Always use `-E`:

```shell
# WRONG — GNU-only
sed -r 's/([0-9]+)/\1/' file.txt

# CORRECT — POSIX-portable
sed -E 's/([0-9]+)/\1/' file.txt
```

### `grep -P` Is GNU-Only PCRE

`grep -P` enables Perl-compatible regex and is unavailable on BSD/macOS. Avoid in portable scripts:

```shell
# WRONG — GNU-only PCRE mode
grep -P '\d+\s+\w+' file.txt

# CORRECT — POSIX extended regex
grep -E '[[:digit:]]+[[:space:]]+[[:alnum:]_]+' file.txt
```

For PCRE features with no ERE equivalent (like `\K` match reset), use `sed -nE 's/.../\1/p'` or a Python snippet.

## Quick Reference

| Non-Portable | Portable Replacement |
|---|---|
| `echo "$var" \| cmd` | `printf '%s\n' "$var" \| cmd` |
| `echo -n "text"` | `printf '%s' "text"` |
| `echo -e "a\nb"` | `printf 'a\nb\n'` |
| `sed -r` | `sed -E` |
| `grep -P` | `grep -E` with POSIX classes |
| `\s` in regex | `[[:space:]]` |
| `\d` in regex | `[[:digit:]]` or `[0-9]` |
| `\w` in regex | `[[:alnum:]_]` |
| `\b` word boundary | `grep -w` or explicit pattern |

## Validation

Portability rules are enforced by `scripts/test_shell_portability.sh` and `tests/ci_config_tests.rs::shell_script_portability`. Run the portability test before committing shell scripts:

```shell
bash scripts/test_shell_portability.sh
```

## See Also

- `ci-configuration.md` "Portable regex" section for additional context
- `ci-configuration.md` "SC2001" section for parameter expansion alternatives to `echo | sed`
