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

## PR Review Gate

If a PR has user-visible changes but no `CHANGELOG.md` update, treat it as
incomplete and add the missing entry before merge.
