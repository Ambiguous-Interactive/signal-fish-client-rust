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
mode. Its built-in `GITHUB_TOKEN` creates `release/X.Y.Z` and its pull request.
GitHub holds the resulting PR workflows for a maintainer to select **Approve
workflows to run**; this approval is required before normal checks execute.

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
and publishes only the dependency-ordered absent set. When a pending dependent
has a checksum-matched published dependency, the plan enables `--no-verify`
only for that resume invocation; the full workspace tests and reproducible
package build have already passed, and this avoids crates.io sparse-index lag.

An existing tag must target current default-branch HEAD. An existing GitHub
Release must have that tag. A mismatch is an integrity incident, not a retryable
error; stop and investigate.

## Repository configuration

Enable **Allow GitHub Actions to create and approve pull requests** in the
repository's Actions settings. Prepare Release requests Contents and Pull
requests write access for its built-in `GITHUB_TOKEN`; no App, personal access
token, repository variable, or release secret is required.

The protected `crates-io` environment holds `CRATES_IO_TOKEN`. Bootstrap new
workspace crates with a token limited to `signal-fish-client*` and
`publish-new` plus `publish-update`; rotate to `publish-update` after the first
publication.

Default-branch rules must match `.github/required-checks.json`. The weekly
Repository Policy workflow detects visible drift with its authenticated
`GITHUB_TOKEN`. GitHub does not expose bypass actors to workflow tokens, so
verify an empty bypass list in the ruleset UI during setup and ownership
reviews. See `docs/releasing.md` for the operator runbook.
