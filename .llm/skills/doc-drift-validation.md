# Documentation Drift Validation

Reference for preventing documentation drift by validating docs against
source-of-truth configuration files.

## The Problem

README files and docs can claim that a config contains keys or behavior that no
longer exists. This drift causes bad setup guidance and CI surprises.

## Validation Pattern

1. Define the keys/settings that matter.
2. Check whether docs mention each key.
3. Verify each mentioned key exists in the real config.
4. Fail with a clear message when docs and config differ.

Example pattern (`scripts/validate-devcontainer-docs.sh`):

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

## Existing Validation in This Repo

| Mechanism | What it catches |
|-----------|----------------|
| `scripts/validate-devcontainer-docs.sh` | Devcontainer README referencing hooks not present in `devcontainer.json` |
| `tests/ci_config_tests.rs` `msrv_consistent_across_key_files` | MSRV drifting between `Cargo.toml` and docs |
| `tests/ci_config_tests.rs` `config_existence` | Config files referenced by CI but missing in repo |

## Checklist for New Docs

- [ ] Is there a validator that cross-references docs against config?
- [ ] Does CI run that validator?
- [ ] For JSONC files, does validation use JSONC-safe matching (for example, `grep`)?

## When to Add a New Validator

Add one when:

- A new config file ships with docs describing its keys.
- A README enumerates settings/hook names from a config file.
- Multiple sources must stay in sync (`Cargo.toml`, workflows, docs).

Implementation guidance:

1. Resolve `REPO_ROOT` from `$SCRIPT_DIR/..`.
2. Prefer text matching that works for JSONC.
3. Exit non-zero on mismatches.
4. Print the doc path and config path in error messages.
