# Release Preparation and Recovery

Reference for the two-stage 0.8+ release automation and its fail-closed
recovery rules.

## Workflow split

`.github/workflows/prepare-release.yml` is the reversible stage. It runs only
by manual dispatch from the default branch, accepts a `major`, `minor`, or
`patch` bump, and supports a dry run. A repository GitHub App creates the
`release/X.Y.Z` branch and pull request so ordinary CI triggers on the generated
change.

`.github/workflows/publish.yml` is the irreversible stage. It runs only by
manual dispatch from the default branch, accepts strict `X.Y.Z`, and uses the
protected `crates-io` environment. Never add a tag-push trigger: a tag is an
output of a verified release, not permission to publish.

## Preparation invariants

Use `python3 scripts/release.py prepare <major|minor|patch>`. The script:

- Requires a clean worktree.
- Calculates the next version without prerelease or build metadata.
- Updates Cargo, dependency snippets, SDK examples, LLM references, the
  compatibility marker, and provenance dates.
- Moves non-empty `[Unreleased]` content into a dated release section and
  updates compare links.
- Fails when an expected reference is absent instead of producing a partial
  release bump.

Update `scripts/test_release.py` whenever the version-reference inventory or
workflow invariants change.

## Publishing order

The release workflow must retain this order:

1. Verify strict input, default-branch HEAD, and successful check runs.
2. Match Cargo and the dated changelog section.
3. Run formatting, Clippy, tests, semver checks, docs.rs simulation, packaging,
   and publish dry-run.
4. Reproduce the `.crate`, SHA-256 manifest, and CycloneDX SBOM.
5. Validate all existing tag, GitHub Release, and crates.io state.
6. Create the annotated tag if absent.
7. Create package provenance and publish if absent.
8. Wait for crates.io to report the reproduced checksum.
9. Create the GitHub Release or repair assets on its matching existing release.

Crates.io versions cannot be overwritten. Never move an existing release tag,
delete state automatically, or publish before package reproduction and dry-run.

## Allowed recovery states

A rerun may proceed when an existing tag targets the current default-branch
SHA or an existing registry version has the exact checksum returned by
`python3 scripts/release.py checksum`. An existing GitHub Release must have the
matching tag.

Any mismatched SHA or checksum is an integrity failure, not a transient CI
failure. Stop and investigate. Recovery may skip an already-matching publish
and upload the reproduced crate, checksum manifest, and SBOM to the matching
GitHub Release with replacement enabled.

## Repository configuration

The release GitHub App needs only Contents and Pull requests read/write access.
Store its client ID in `RELEASE_APP_CLIENT_ID` and private key in
`RELEASE_APP_PRIVATE_KEY`. Store the crate-scoped token as `CRATES_IO_TOKEN` in
the protected `crates-io` environment, with required reviewers and a default
branch deployment restriction.

See `docs/releasing.md` for the operator-facing runbook.
