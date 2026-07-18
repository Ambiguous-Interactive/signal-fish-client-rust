---
name: release-recovery
description: Operate and recover the two-stage Signal Fish release process. Use when preparing a release, publishing, retrying failed automation, cleaning tags, or deciding safe recovery steps.
---

# Release Preparation and Recovery

Reference for lockstep, workspace-discovered publication and fail-closed
recovery.

## Workflow split

`.github/workflows/prepare-release.yml` is reversible. Manual dispatch from the
default branch accepts a version bump, deliberate breaking marker, and dry-run
mode. A repository GitHub App creates `release/X.Y.Z` and its pull request so
all normal PR workflows run.

`.github/workflows/publish.yml` is irreversible. It is manual-only, has no
version input, derives the version from the merged workspace, and uses the
protected `crates-io` environment. A tag is an output, never a publish trigger.

## Workspace and preparation invariants

`[workspace.package].version` is authoritative. Every crates.io-publishable
member uses `version.workspace = true` and `publish = ["crates-io"]`. Internal
publishable dependencies inherit an exact `=X.Y.Z` requirement from
`[workspace.dependencies]`.

`python3 scripts/release.py workspace-plan` uses `cargo metadata` to discover
eligible members, reject version or dependency-policy drift, reject cycles and
dependencies on non-publishable workspace crates, and return a deterministic
dependency-first plan. It also reads each member manifest to require
`workspace = true`; metadata's resolved exact requirement alone cannot prove
that preparation will update the member on the next version bump.

`python3 scripts/release.py prepare <major|minor|patch>` validates the complete
inventory before writing, changes the shared version and exact requirements,
updates locks, documentation, compatibility and provenance markers, and cuts a
non-empty changelog release. Update `scripts/test_release.py` when a release
invariant changes.

## Publishing order

The release workflow must retain this order:

1. Verify default-branch HEAD and every check in
   `.github/required-checks.json`.
2. Derive the workspace plan and version; validate the dated changelog.
3. Run formatting, Clippy, tests, per-crate semver checks, and docs.rs checks.
4. Package every planned crate together with pinned Rust 1.96.1; create each
   SBOM and the SHA-256 manifest.
5. Revalidate HEAD, tag, GitHub Release, and exact crates.io checksums.
6. Require the crates.io token and dry-run only the unpublished plan.
7. Attest packages, create the tag if absent, and publish pending crates in
   dependency order.
8. Wait for and verify every checksum, then create or repair the GitHub Release.

Never move a tag, overwrite a crate version, delete release state
automatically, or publish bytes that differ from the locally reproduced
artifact.

## Allowed recovery states

`registry-plan` classifies each package as `unpublished` or
`published-matching`. It rejects a checksum mismatch and rejects any published
dependent whose internal dependency is absent. A rerun skips matching packages
and publishes only the dependency-ordered absent set.

An existing tag must target current default-branch HEAD. An existing GitHub
Release must have that tag. A mismatch is an integrity incident, not a retryable
error; stop and investigate.

## Repository configuration

The release App needs Contents and Pull requests read/write access plus
Administration read access for authenticated ruleset audits. Store its client
ID in `RELEASE_APP_CLIENT_ID` and PEM key in
`RELEASE_APP_PRIVATE_KEY`. The prepare preflight diagnoses either missing value.

The protected `crates-io` environment holds `CRATES_IO_TOKEN`. Bootstrap new
workspace crates with a token limited to `signal-fish-client*` and
`publish-new` plus `publish-update`; rotate to `publish-update` after the first
publication.

Default-branch rules must match `.github/required-checks.json`. The weekly
Repository Policy workflow detects drift with an App token explicitly scoped to
Administration read. Never make live audit requests anonymously. See
`docs/releasing.md` for the operator runbook.
