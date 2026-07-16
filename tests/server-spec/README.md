# Vendored server AsyncAPI protocol spec

This YAML file is copied verbatim from the Signal Fish **server** repository
(`spec/signal-fish-protocol.asyncapi.yaml`) and is the source of truth for the
[`tests/error_code_conformance_tests.rs`](../error_code_conformance_tests.rs)
suite, which proves the client's `ErrorCode` enum stays in lockstep with the
error codes the server may actually send.

- The spec's `ErrorCode` schema lists every wire error-code token
  (SCREAMING_SNAKE_CASE). The conformance suite extracts that enum block with
  a plain line scan (no YAML dependency) and asserts, in both directions, that
  every server token deserializes into a client `ErrorCode` variant and every
  client variant serializes to a token the spec declares.

Provenance — the exact upstream commit and the file's SHA-256 checksum — is
recorded in [`PROVENANCE.toml`](PROVENANCE.toml) and verified by a policy test
in `tests/ci_config_tests.rs`.

## Refreshing when the server protocol changes

See [`.llm/skills/protocol-wire-conformance/SKILL.md`](../../.llm/skills/protocol-wire-conformance/SKILL.md)
for the full procedure. In short:

1. Copy `spec/signal-fish-protocol.asyncapi.yaml` from the server repo into
   this directory.
2. Update `PROVENANCE.toml`: the upstream `commit`, the `synced` date, and
   recompute the `[files]` SHA-256 sum (`sha256sum *.yaml`).
3. Run `cargo test --test error_code_conformance_tests`. If it is red, add the
   missing variants to `src/error_codes.rs` until it is green — **never** edit
   the spec to pass.
