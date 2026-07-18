# Release Operations

Signal Fish Client uses separate preparation and publication workflows. The
preparation stage is reversible and reviewable; the publication stage is
manual, protected, and fail-closed.

## One-time repository setup

In **Settings > Actions > General > Workflow permissions**, enable **Allow
GitHub Actions to create and approve pull requests**. Prepare Release uses only
the run's built-in `GITHUB_TOKEN`, explicitly scoped to **Contents: write** and
**Pull requests: write**. It requires no GitHub App, personal access token,
repository variable, or release secret.

GitHub places workflows triggered by a pull request created with
`GITHUB_TOKEN` into an approval-required state. After Prepare Release opens the
pull request, a maintainer with write access must select **Approve workflows to
run**. This manual approval is the deliberate app-free replacement for an App
token that could trigger checks automatically.

Configure the protected `crates-io` environment with required reviewers, a
default-branch deployment restriction, and a `CRATES_IO_TOKEN` secret. For the
first adapter release, create a crates.io token scoped to the
`signal-fish-client*` crate pattern with both `publish-new` and
`publish-update`. After every crate has been published once, rotate it to
`publish-update` only. Artifact attestations must also be enabled.

Protect the default branch with an active ruleset that has no bypass actors and
requires:

- pull requests, one approval, stale-review dismissal, and resolved threads;
- a branch updated with its base before merging;
- every job named in `.github/required-checks.json`;
- deletion and non-fast-forward protections.

The weekly **Repository Policy** workflow audits the live, publicly readable
ruleset fields against this checked-in policy using its built-in authenticated
`GITHUB_TOKEN`. GitHub returns `bypass_actors` only to credentials with write
access to the ruleset, which workflow tokens cannot request. Verify in the
ruleset UI that **Bypass list** is empty during initial setup and repository
ownership reviews; the workflow intentionally does not claim to automate that
hidden check. For a local live audit, set `GH_TOKEN` to an authenticated token
with repository Metadata read access, then run
`python3 scripts/audit-repository-rules.py`. Offline fixture audits with
`--rulesets FILE` do not require a token.

## Prepare a release

1. Run **Prepare Release** from the default branch with `dry_run` enabled.
2. Select `major`, `minor`, or `patch`. Enable `breaking` only for an
   intentional major release or pre-1.0 breaking minor release.
3. Inspect the generated diff and validation output.
4. Run it again with `dry_run` disabled. The built-in token creates
   `release/X.Y.Z` and opens the preparation pull request.
5. Open the pull request and select **Approve workflows to run**.
6. Merge only after every aggregate required check, review, and thread is
   green.

The workspace owns one version at `[workspace.package].version`; publishable
members set `version.workspace = true`. Preparation discovers crates through
`cargo metadata`, verifies internal publishable dependencies inherit through
`workspace = true`, updates the shared version and exact workspace
requirements by manifest key (including renamed dependencies), then updates
locks, documentation references, provenance, and the changelog. It fails
before writing if the workspace graph, inventory, or `[Unreleased]` section is
invalid.

```sh
python3 scripts/release.py workspace-plan
python3 scripts/release.py prepare minor
```

## Publish a release

Run **Release** from the default branch. It has no version input: the workflow
derives the strict lockstep version and dependency-first package order from the
merged workspace. The protected-environment approval is the authorization to
publish.

The workflow verifies default-branch HEAD and all configured aggregate checks,
runs the complete Rust, semver, and docs.rs suites, and uses Cargo's
multi-package support to package every crates.io-publishable workspace member.
It creates one `.crate` and CycloneDX SBOM per discovered package plus one
checksum manifest. The release jobs use pinned Rust 1.96.1 and Ubuntu 24.04 so
multi-package behavior cannot drift with `stable` or `ubuntu-latest`.

Before mutation, `registry-plan` queries every exact crate version. It publishes
only absent packages, in dependency order, and verifies every resulting
registry checksum. One annotated tag, attestation set, and GitHub Release cover
the whole workspace release.

## Recovery rules

Re-run **Release** after a transient failure; do not enter or change a version.
A rerun proceeds only when:

- an existing tag targets the current default-branch SHA;
- every published crate checksum equals the locally reproduced `.crate`;
- a published dependent never has an unpublished internal dependency;
- any existing GitHub Release has the matching tag.

Matching registry packages are skipped, absent packages are resumed, and an
existing matching GitHub Release has its assets repaired. A tag mismatch,
checksum mismatch, impossible dependency state, missing required check, or a
default-branch move stops the run. Never delete or move release state to make a
rerun pass. If only a dependent remains unpublished, the rerun uses
`--no-verify` after full workspace verification and exact dependency checksum
matching so crates.io sparse-index propagation cannot strand recovery.

After success, confirm crates.io and docs.rs show every planned package and
verify each downloaded crate attestation:

```sh
gh attestation verify signal-fish-client-X.Y.Z.crate \
  --repo Ambiguous-Interactive/signal-fish-client-rust
gh attestation verify signal-fish-client-godot-X.Y.Z.crate \
  --repo Ambiguous-Interactive/signal-fish-client-rust
```
