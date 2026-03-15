# Changelog Discipline

Reference for deciding when `CHANGELOG.md` must be updated for user-visible changes.

## Policy

If a change is user-visible, update `CHANGELOG.md` in the same PR under
`## [Unreleased]`.

User-visible means any change that can affect crate consumers, including:

- Public API additions, removals, or signature/type changes
- Behavior changes in existing APIs (including validation and edge cases)
- New feature flags or feature-flag default changes
- Error model changes (`SignalFishError`, `ErrorCode`, or error text contracts)
- Protocol/wire-format behavior changes
- Dependency or MSRV changes that affect downstream users
- Documentation changes that alter recommended usage or migration steps

Do not add changelog entries for internal-only refactors that preserve behavior
and public surface area.

Internal implementation details must stay out of `CHANGELOG.md`, including:

- CI/workflow/script changes
- Pre-commit or release automation updates
- Test-only additions/refactors
- Internal code cleanup with no observable behavior/API change

## Required Workflow Step

Before finalizing a user-visible change:

1. Classify whether the change is user-visible.
2. Add or update an entry in `CHANGELOG.md` under `## [Unreleased]`.
3. Place the entry in the right section (`Added`, `Changed`, `Deprecated`,
   `Removed`, `Fixed`, `Security`).
4. Use concrete, consumer-facing wording (what changed and impact).

## Section Classification Rules

- New public API surface area belongs under `### Added`.
- Bug fixes and behavioral corrections to existing APIs belong under `### Fixed`
  or `### Changed` as appropriate.
- Do not hide API additions inside `### Changed`/`### Fixed`; list each new
  field/method/type explicitly under `### Added` (including defaults when they
  affect behavior).
- `### Changed` is only for features that existed in a prior release. If a feature was added in the same version, its behavior descriptions belong under `### Added`, not `### Changed`.

## Classification Examples

### Must update changelog

- Added new public method on `SignalFishClient`
- Changed JSON serialization shape of any protocol type
- Added new `SignalFishEvent` variant
- Changed default `SignalFishConfig` behavior
- Fixed bug that changes observable runtime behavior
- Changed target-specific dependency features in a way that affects supported targets (for example enabling WASM-specific `uuid` features)

### Usually does not require changelog

- Test-only refactors
- CI/script cleanup with no user impact
- Internal implementation cleanup with identical behavior
- Typo fixes that do not change meaning

## Version Consistency Rule

When bumping the version in `Cargo.toml`, you **must** also create a matching
`## [x.y.z] - YYYY-MM-DD` section in `CHANGELOG.md` and move relevant
`[Unreleased]` items into it. The test
`changelog_has_entry_for_cargo_version_when_not_unreleased_only` enforces this:
if `Cargo.toml` version is ahead of the latest released CHANGELOG entry, the
test fails.

Either:

1. Bump `Cargo.toml` version **and** add the dated CHANGELOG section together, or
2. Keep `Cargo.toml` at the current released version until release cutover.

Never leave `Cargo.toml` at a new version with changes still under `[Unreleased]`.

## PR Review Gate

If a PR has user-visible changes but no `CHANGELOG.md` update, treat it as
incomplete and add the missing entry before merge.
