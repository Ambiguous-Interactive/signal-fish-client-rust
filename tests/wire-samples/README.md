# Vendored protocol wire samples

These JSONL files are copied verbatim from the Signal Fish **server** repository
(`.llm/code-samples/protocol/`) and are the source of truth for the
[`tests/wire_golden_tests.rs`](../wire_golden_tests.rs) conformance suite, which
proves the client's wire types stay wire-compatible with the server.

- **`v3-*`** samples are complete (real ids, all fields) and get full semantic
  round-trip conformance: each line must deserialize into our typed enum and
  re-serialize to a semantically identical `serde_json::Value` (compared as
  parsed JSON, so key order and whitespace are ignored).
- **`v2-*`** samples are illustrative documentation (they elide optional fields
  and use `"..."` placeholders for ids), so they get a structural sanity check
  only. The v2 wire format is byte-tested directly with complete messages in
  [`tests/protocol_tests.rs`](../protocol_tests.rs).

Provenance — the exact upstream commit and per-file SHA-256 checksums — is
recorded in [`PROVENANCE.toml`](PROVENANCE.toml) and verified by a policy test in
`tests/ci_config_tests.rs`.

## Refreshing when the server protocol changes

See [`.llm/skills/protocol-wire-conformance.md`](../../.llm/skills/protocol-wire-conformance.md)
for the full procedure. In short:

1. Copy the four `.jsonl` files from the server repo's
   `.llm/code-samples/protocol/` into this directory.
2. Update `PROVENANCE.toml`: the upstream `commit`, the `synced` date, and
   recompute the `[files]` SHA-256 sums (`sha256sum *.jsonl`).
3. Run `cargo test --test wire_golden_tests`. If it is red, fix the client types
   in `src/protocol.rs` until it is green — **never** edit the samples to pass.
