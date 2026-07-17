# Session 001: Godot adapter split

## Objective

Move the concrete Godot integration into a lockstep
`signal-fish-client-godot` crate, keep the core crate independent at Rust 1.87,
prove godot-rust 0.4.5 and 0.5.4 compatibility, and publish the result as a
fully green pull request with all automated review feedback addressed.

## Progress

- Split the existing transport implementation and its 34 behavioral tests into
  the companion crate without changing its public type names or behavior.
- Removed the core Godot feature, dependency, module, and re-exports.
- Added exact minimum/latest standalone fixtures, committed lockfiles, and
  policy tests for version ranges, lockstep versions, and binding unification.
- Updated documentation, CI, Dependabot, changelog, LLM guidance, and two-crate
  release/recovery automation.
- Passed the mandatory workspace format, Clippy, and test workflow locally.
- Verified the packaged core with Rust 1.87 and a metadata graph containing no
  Godot-family packages.
- Started independent adversarial audits of implementation and release/CI
  behavior before committing and publishing.
- Fixed every concrete audit finding: unpublished exact-core packaging now uses
  a local Cargo patch in CI and release workflows; the repository-only adapter
  policy test is excluded from the core package; Fortress documents the full
  dependency/MSRV/type-identity contract; and the WASM MSRV wording separates
  core from adapter.
- Strengthened the minimum fixture to construct a real core polling client
  around the directly-created Godot peer, eliminating an unused dependency and
  proving the documented downstream dependency shape.
- Reproduced a prepared 0.9.0 tree in a temporary directory and confirmed that
  both core and adapter packages succeed before core 0.9.0 exists on crates.io.
- Reused the identical local-core patch for final adapter dry-run/publication so
  Cargo republishes the already-reproduced artifact instead of changing its
  lock source and checksum after core becomes registry-visible.

## Remaining

- Commit and push the complete scoped worktree.
- Open the pull request, explicitly request automated reviews, and await all CI
  checks and reviewer feedback.
- Fix every actionable failure or review thread until the PR is fully green.
