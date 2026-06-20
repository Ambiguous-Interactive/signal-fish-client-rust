# Protocol Wire Conformance

Reference for the vendored golden wire samples, the provenance marker, and the procedure for keeping the client byte-compatible with the Signal Fish server as the protocol evolves.

## What Is Vendored

The server publishes literal wire samples at
`.llm/code-samples/protocol/{v2,v3}-{client,server}-messages.jsonl`. These are
vendored verbatim into `tests/wire-samples/` and consumed by
`tests/wire_golden_tests.rs`:

- **`v3-*`** samples are complete (real ids, all fields) → full byte-exact
  round-trip conformance: each line must deserialize into our typed enum AND
  re-serialize to a semantically identical `serde_json::Value`.
- **`v2-*`** samples are illustrative docs (placeholders / partial payloads) →
  structural check only (valid JSON + string `type`). v2 bytes are tested
  directly with complete messages in `tests/protocol_tests.rs`.

`tests/wire-samples/PROVENANCE.toml` records the upstream commit, the protocol
version range, the sync date, and a SHA-256 per file.

## The Refresh Procedure

When the server protocol changes, refresh the vendored corpus:

1. Copy the four `.jsonl` files from the server repo's
   `.llm/code-samples/protocol/` into `tests/wire-samples/`.
2. Update `PROVENANCE.toml`: the upstream `commit`, the `synced` date, and
   recompute the `[files]` checksums (`sha256sum tests/wire-samples/*.jsonl`).
3. Run `cargo test --test wire_golden_tests`. If red, update the client types in
   `src/protocol.rs` until green — **never edit the samples to make a test pass.**
   The JSONL is the source of truth; the types adapt.
4. Update `CHANGELOG.md` (`### Added` for new variants/fields; `### Changed` only
   if a pre-existing v2 field changed). Bump the version per
   [crate-publishing](crate-publishing.md).

## Guard Tests

In `tests/ci_config_tests.rs` (`protocol_wire_conformance_policy`):

- `wire_sample_files_exist_and_are_non_empty` — the corpus is present.
- `wire_provenance_marker_is_valid` — the marker has a 40-hex `commit`, version
  range, `synced` date, and a 64-hex checksum per file.
- `wire_provenance_checksums_match_vendored_files` — recomputes each SHA-256, so a
  sample cannot be edited without updating the marker (keeps provenance honest).
- `no_protocol_type_uses_deny_unknown_fields` — protocol types must stay
  forward-compatible with additive server fields.

## Drift Detection

`.github/workflows/protocol-sync.yml` runs weekly: it re-fetches the upstream
samples and **fails loudly** if the vendored copies have drifted from the recorded
commit (fail-closed, no auto-PR). When it fails, follow the refresh procedure
above. This catches the "upstream changed but nobody refreshed us" case that the
offline checksum test cannot.

## Why Fail-Closed

The protocol is purely additive (the relay floor is byte-frozen), so drift is a
human signal to reconcile, not something to auto-apply. A red scheduled run points
the operator here; a human runs the refresh and reviews any type changes. See
[protocol-versioning-and-negotiation](protocol-versioning-and-negotiation.md) for
the relay-floor guarantee that the conformance suite protects.
