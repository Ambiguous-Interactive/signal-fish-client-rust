# Keep a Changelog Format

Reference for writing `CHANGELOG.md` entries in Keep a Changelog format for this crate.

## Required Structure

Keep entries under `## [Unreleased]` with these section headings:

- `### Added`
- `### Changed`
- `### Deprecated`
- `### Removed`
- `### Fixed`
- `### Security`

Only include sections that have entries.

## Entry Writing Rules

- Write for crate consumers, not internal implementation details.
- Exclude internal-only maintenance (scripts, CI wiring, pre-commit automation, test-only refactors).
- Use one bullet per distinct change.
- Start bullets with the user-facing outcome, then include technical detail.
- Use backticks for API/type/variant names.
- Include migration guidance when behavior or API expectations changed.
- Keep wording specific; avoid vague bullets like "improved things."

## Style Examples

Good:

- Added `SignalFishClient::ping` to allow explicit heartbeat requests from clients.
- Changed `SignalFishError::ServerError.error_code` to `Option<ErrorCode>`.
- Fixed client shutdown race where `Disconnected` could be emitted twice.

Weak:

- Updated client internals.
- Updated pre-commit version-sync script.
- Fixed issues.
- Refactor networking.

## Release Cutover Rules

When releasing:

1. Move relevant items from `Unreleased` into a new version section:
   `## [x.y.z] - YYYY-MM-DD`
2. Keep category headings and bullets intact.
3. Update compare links at the bottom of `CHANGELOG.md`.

Example links:

```markdown
[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.2.1...v0.2.2
```

## Quality Check

Before merge, verify:

1. Every user-visible PR change is represented.
2. Section choice matches the actual change type.
3. Wording is understandable without reading the PR diff.
