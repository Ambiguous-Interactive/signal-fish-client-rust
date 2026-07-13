# Release Operations

Signal Fish Client releases use two manually dispatched workflows. The split
keeps ordinary version-file changes reviewable before the irreversible
crates.io publish.

## One-time repository setup

Create and install a GitHub App with repository **Contents: read and write** and
**Pull requests: read and write** permissions. Add its client ID as the
`RELEASE_APP_CLIENT_ID` repository variable and its private key as the
`RELEASE_APP_PRIVATE_KEY` repository secret. The App token is required because
branches and pull requests created with the normal workflow token do not
reliably trigger the repository's full CI policy.

Configure the protected `crates-io` environment with required reviewers and a
`CRATES_IO_TOKEN` secret scoped to the `signal-fish-client` crate. Restrict the
environment to the default branch. Artifact attestations also require GitHub
Actions and attestations to be enabled in repository settings.

## Prepare a release

1. Run **Prepare Release** from the default branch.
2. Select `major`, `minor`, or `patch` and leave `dry_run` enabled first.
3. Inspect the generated diff and validation output.
4. Run it again with `dry_run` disabled. The workflow creates
   `release/X.Y.Z`, updates every version and provenance marker, cuts the
   changelog, and opens a pull request.
5. Review and merge only after all required CI and reviewer feedback is green.

Preparation fails if the default branch is not selected, the worktree is not
clean, a version reference is missing, or `[Unreleased]` has no content. The
underlying deterministic command is:

```sh
python3 scripts/release.py prepare minor
```

## Publish a release

Run **Release** from the default branch and enter the strict `X.Y.Z` version
from the merged preparation pull request. After the protected-environment
approval, the workflow verifies default-branch HEAD and its checks, package and
changelog versions, the full Rust suite, docs.rs compatibility, semver policy,
and `cargo publish --dry-run`.

The workflow then reproduces the `.crate`, checksum manifest, and CycloneDX
SBOM; creates an annotated `vX.Y.Z` tag; attests the package; publishes to
crates.io; waits until the registry reports the exact package checksum; and
creates the GitHub Release with all three assets.

## Recovery rules

Re-run **Release** with the same version after a transient failure. Recovery is
allowed only when every existing artifact agrees with the current
default-branch commit:

- An existing tag must target the current SHA.
- An existing crates.io version must have the exact checksum of the locally
  reproduced package.
- An existing GitHub Release must have the matching tag.

The workflow fails closed on any mismatch. It never overwrites a crate version;
when registry publication already matches, it skips publishing and repairs only
the GitHub Release assets. If a tag or registry checksum points elsewhere, stop
and investigate rather than deleting or moving release state.

After success, confirm the version and assets on crates.io and GitHub, confirm
docs.rs built the same version, and verify the package attestation with:

```sh
gh attestation verify signal-fish-client-X.Y.Z.crate \
  --repo Ambiguous-Interactive/signal-fish-client-rust
```
